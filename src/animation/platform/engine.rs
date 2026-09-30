//! Drives the capture-based animation overlay: owns the overlay and the snapshot cache.
//! Runs on the main thread because Core Animation requires it.
//!
//! Design in `src/animation/docs/animation-smoothness.md`; measurements in `src/animation/docs/capture-overlay-research.md`.

use crate::animation::domain::admission::*;
use crate::animation::domain::flight::*;
use crate::animation::domain::request::AnimationRequest;
use crate::animation::domain::timing::*;
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_foundation::MainThreadMarker;
use tracing::{debug, warn};

use crate::animation::domain::request::SnapshotTarget;
use crate::animation::platform::overlay::{OverlayTile, TileOverlay};
use crate::animation::platform::snapshot_service::SnapshotService;
use crate::animation::platform::window_snapshot::{
    SnapshotCache, WindowSnapshot, capture_via_framed_with_dressing, is_wholly_on_a_display,
};
use rini_core::ids::WindowId;
use rini_core::ids::WindowServerId;
use rini_geometry::SameAs;
use rini_runloop::channel;
use rini_runloop::run_loop::RepeatingTimer;

pub(crate) use crate::animation::domain::motion::plan;
use crate::animation::domain::motion::strip_move::NUDGE_STRETCH;
pub use crate::animation::domain::motion::strip_move::Nudge;
pub use crate::animation::domain::motion::surface::SurfaceWindow;
pub(crate) use crate::animation::domain::motion::surface::{pan_travel, to_overlay_space};
use crate::animation::domain::motion::travel::{TilePath, is_moving, tile_path};

/// One window's part in an animation, as the caller describes it.
#[derive(Debug)]
pub enum Event {
    /// Animate a set of windows. The caller must have already placed the real windows at their
    /// final frames, or arrange to do so immediately after sending this.
    Animate {
        windows: Vec<AnimationRequest>,
        focus: Option<WindowId>,
        duration: Duration,
    },
    /// Display geometry for the overlay: the display's whole bounds. `picture_bar` says whether a
    /// bar under the overlay is pictured and drawn over flights; it is off while rini draws its own
    /// bar above the overlay, which leaves nothing there to picture.
    SetDisplay {
        id: u32,
        frame: CGRect,
        scale: f64,
        picture_bar: bool,
        /// Points to leave clear at the top for the bar; the overlay window starts below it.
        top_band: f64,
    },
    /// Drop snapshots for windows that no longer exist, so the cache cannot grow without bound.
    ForgetWindow(WindowId),
    /// Slide every currently visible window in from an offset, purely to evaluate animation quality
    /// by eye. Does not touch any real window, so it is safe to fire at any time.
    DebugSlide {
        dx: f64,
        dy: f64,
        duration: Duration,
    },
    /// Move the whole strip surface by one travel, as one rigid group; a leaving window animates
    /// off screen while its real frame parks, and the two columns a move swapped cross the surface
    /// from their old slots. See "Strip movements" in `src/animation/docs/animation-smoothness.md`.
    AnimateSurface {
        windows: Vec<SurfaceWindow>,
        from_offset: CGPoint,
        to_offset: CGPoint,
        /// Real screen frames to apply once the overlay is covering them.
        final_frames: Vec<(WindowId, CGRect)>,
        /// The window that will hold focus once this settles, drawn in front of the rest.
        focus: Option<WindowId>,
        duration: Duration,
        /// A moved window's step toward where it went and back; drawn only.
        nudge: Option<Nudge>,
    },
    /// Nudge the strip surface by `overshoot` and bring it back; real windows stay put. Rides an
    /// in-flight movement additively. See "Edge bounce" in `src/animation/docs/animation-smoothness.md`.
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
    PictureReady {
        window: WindowId,
        snapshot: WindowSnapshot,
        settled: bool,
    },
    /// A hairline harvest finished. Harvested off the capture service's completion queue, which
    /// the framed capture behind it would deadlock (see `snapshot_service`).
    DressingReady {
        window: WindowId,
        dressing: crate::animation::platform::edge_dressing::EdgeDressing,
    },
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
    /// Hand out the pictures already cached for these windows, to whoever installed `lend_snapshots`.
    ///
    /// A READ. It captures nothing and queues nothing, so it costs a hash lookup per window and is
    /// safe to send on a keypress — which is the point, since the switcher popup cannot afford the
    /// 40ms plus 14.5ms per window a capture costs.
    ///
    /// Answered through an installed callback rather than a reply channel, the way `place_frames`
    /// already is: the animation feature must not name whoever is asking.
    LendSnapshots(Vec<WindowId>),
}

/// Called with real-window frames to apply while the overlay covers them.
pub type PlaceFrames = Box<dyn Fn(Vec<(WindowId, CGRect)>)>;

/// Called with `true` when an overlay goes up and `false` once flying has settled; see
/// `domain::flight::FlightReport`.
pub type OnFlight = Box<dyn Fn(bool)>;

/// Called with the pictures this engine holds for the windows `LendSnapshots` asked about.
///
/// Only windows with a picture worth drawing appear; the rest are absent rather than present and
/// empty, so the caller does not have to know what makes a snapshot usable.
pub type LendSnapshots = Box<dyn Fn(Vec<(WindowId, WindowSnapshot)>)>;

pub type Sender = channel::Sender<Event>;
pub type Receiver = channel::Receiver<Event>;

/// The focus refresh's capture targets, one per wanted window that has a tile, and the windows covered.
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
    /// The recapture of the focus change's two ends. See `refresh_destination_among`.
    refresh: FocusRefresh,
    /// Windows whose hairline landed this flight, so `finish` harvests nothing twice.
    harvested: HashSet<WindowId>,
    /// The window gaining focus, from the latest pass that named one; its group is banded in front.
    focus: Option<WindowId>,
    /// The flight as rigid pieces: what `install` composed and `fly` animates.
    /// See "The overlay engine" in `src/animation/docs/animation-smoothness.md`.
    plan: plan::FlightPlan,
    /// A move's nudge waiting for the flight to move, with the move's plain duration
    /// (`NudgeAdmission::Wait`).
    nudge: Option<(Nudge, Duration)>,
    /// Dropped when the animation ends, which invalidates the timer.
    _clock: Option<RepeatingTimer>,
}

/// Whether `finish` should ask for a new desktop render: missing, for another display, or stale.
fn desktop_render_wanted(render: Option<(Duration, (f64, f64))>, display: (f64, f64)) -> bool {
    match render {
        None => true,
        Some((age, covered)) => {
            !crate::animation::platform::window_snapshot::spans_display(covered, display)
                || crate::animation::platform::window_snapshot::picture_is_stale(age)
        }
    }
}

/// The thumbprint of a snapshot's bitmap; `None` for a surface, which cannot be compared.
fn bitmap_thumbprint(snapshot: &WindowSnapshot) -> Option<Vec<u8>> {
    match &snapshot.image {
        crate::animation::platform::window_snapshot::SnapshotImage::Bitmap(image) => {
            crate::animation::platform::edge_dressing::thumbprint(image)
        }
        _ => None,
    }
}

/// Writes every tile's destination back from the merged plan: a member the pass did not compose
/// rides its group all the same.
fn sync_tiles_to_plan(tiles: &mut [OverlayTile], plan: &plan::FlightPlan) {
    for tile in tiles.iter_mut() {
        let Some(member) = plan.member(tile.window) else {
            continue;
        };
        tile.to = match member {
            plan::Member::Rigid { key, rel } => plan::overlay_of(rel, plan.position_of(key)),
            plan::Member::Changing { to, .. } | plan::Member::Entrance { to, .. } => to,
            plan::Member::Floating { to, .. } => {
                plan::overlay_of(to, plan.position_of(plan::GroupKey::Floating))
            }
        };
    }
}

/// The tile of a border window at `border`'s frame tracing `anchor`, whose real frame is `real`: it
/// follows the anchor's tile at the same offset, at its depth and in its band, a floating window's
/// border in the floating container with it. See "Window borders during animations" in
/// `src/animation/docs/animation-smoothness.md`.
fn border_tile(
    anchor: &OverlayTile,
    real: CGRect,
    border: (WindowId, CGRect),
    snapshot: WindowSnapshot,
) -> OverlayTile {
    let (window, frame) = border;
    let offset = CGPoint::new(frame.origin.x - real.origin.x, frame.origin.y - real.origin.y);
    let follow = |rect: CGRect| {
        CGRect::new(
            CGPoint::new(rect.origin.x + offset.x, rect.origin.y + offset.y),
            frame.size,
        )
    };
    OverlayTile {
        window,
        from: follow(anchor.from),
        to: follow(anchor.to),
        snapshot,
        floating: anchor.floating,
        server_order: None,
        // Its window's banded depth; `restack` leaves companions alone.
        depth: anchor.depth,
        companion: Some(anchor.window),
        focused: false,
    }
}

/// The stand-ins a flight that does not hold chases the real pictures of (`chases_stand_in`), with
/// the size a picture must fit; `viewport` is the display in overlay space. The reveal swap puts
/// the picture on the same tile mid-flight.
fn stand_in_chase(tiles: &[OverlayTile], viewport: CGRect) -> Vec<(WindowId, CGSize)> {
    tiles
        .iter()
        .filter(|tile| {
            tile.snapshot.source
                == crate::animation::platform::window_snapshot::SnapshotSource::Placeholder
                && chases_stand_in(tile.to, viewport)
        })
        .map(|tile| (tile.window, tile.to.size))
        .collect()
}

/// A move's nudge arriving at a flight that does not move yet, kept with the move's plain duration
/// until it does (`NudgeAdmission::Wait`); `None` when it is skipped.
fn nudge_waiting(
    nudge: Option<Nudge>,
    duration: Duration,
    playing: &OutAndBacks,
    now: Instant,
) -> Option<(Nudge, Duration)> {
    let nudge = nudge?;
    match nudge_admission(playing, now, false, None) {
        NudgeAdmission::Wait => Some((nudge, duration)),
        admission => {
            admitted(nudge, admission);
            None
        }
    }
}

/// The container a move's nudge starts on, or `None`, the reason logged when it is skipped.
fn admitted(nudge: Nudge, admission: NudgeAdmission) -> Option<(Nudge, plan::GroupKey)> {
    match admission {
        NudgeAdmission::Start(key) => Some((nudge, key)),
        NudgeAdmission::Wait => None,
        NudgeAdmission::Skip(reason) => {
            debug!(reason, "move nudge skipped");
            None
        }
    }
}

/// Steps a moved window out by its nudge and back on container `key` over `duration`, and holds the
/// flight's clock and lift for it to come home. See "The move flight" in
/// `src/animation/docs/animation-smoothness.md`.
fn nudge_on(
    overlay: &mut TileOverlay,
    running: &mut RunningAnimation,
    playing: &mut OutAndBacks,
    nudge: Nudge,
    key: plan::GroupKey,
    duration: Duration,
) {
    playing.start(OutAndBack::Nudge, Instant::now(), duration);
    running.plan.nudging = Some(nudge.window);
    running.duration = clock_for_bounce(running.started, running.duration, duration);
    overlay.nudge(key, nudge.offset, duration);
    debug!(
        offset = format!("{:.0},{:.0}", nudge.offset.x, nudge.offset.y),
        key = format!("{key:?}"),
        "move nudge"
    );
}

/// The capture work a lift leaves for the quiet period.
#[derive(Default)]
struct AfterFlight {
    targets: Vec<SnapshotTarget>,
    harvested: HashSet<WindowId>,
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
        companion: None,
        focused: true,
    }
}

/// The z rule's view of every tile that is not a companion, in tile order.
fn stacked_windows(
    tiles: &[OverlayTile],
) -> Vec<crate::animation::domain::motion::z_group::Stacked> {
    tiles
        .iter()
        .filter(|t| t.companion.is_none())
        .map(|t| crate::animation::domain::motion::z_group::Stacked {
            window: t.window,
            group: group_of(t.floating),
            server_order: t.server_order,
        })
        .collect()
}

/// Depth for every tile, banded by `z_group::stack`; companions keep their window's depth.
/// The reactor's regroup matches it. See "Mid-flight passes" in `src/animation/docs/animation-smoothness.md`.
fn restack(tiles: &mut [OverlayTile], focus: Option<WindowId>) {
    let placements =
        crate::animation::domain::motion::z_group::stack(&stacked_windows(tiles), focus);
    for (tile, placement) in tiles.iter_mut().filter(|t| t.companion.is_none()).zip(placements) {
        tile.depth = placement.depth;
    }
}

/// The flight's z-order as containers: `container_z - within` reproduces `-depth`.
/// See "The overlay engine" in `src/animation/docs/animation-smoothness.md`.
fn band_plan(
    plan: &plan::FlightPlan,
    tiles: &[OverlayTile],
    focus: Option<WindowId>,
) -> plan::Banding {
    use crate::animation::domain::motion::z_group::{
        Band, GROUP_STRIDE, focus_is_off_strip, stack,
    };
    let stacked = stacked_windows(tiles);
    let placements = stack(&stacked, focus);
    let mut within: HashMap<WindowId, usize> = HashMap::default();
    let mut lifted: Vec<WindowId> = Vec::new();
    for (window, placement) in stacked.iter().zip(&placements) {
        within.insert(window.window, placement.within);
        if placement.band == Band::Lifted {
            lifted.push(window.window);
        }
    }
    for tile in tiles.iter().filter(|t| t.companion.is_some()) {
        // Its window's banded depth, less the band, and its window's band.
        within.insert(tile.window, tile.depth % GROUP_STRIDE);
        if tile.companion.is_some_and(|anchor| lifted.contains(&anchor)) {
            lifted.push(tile.window);
        }
    }
    let mut strip: Vec<(plan::GroupKey, bool, usize)> = Vec::new();
    for group in plan.groups.iter().filter(|g| !g.members.is_empty()) {
        let holds_focus = focus.is_some_and(|f| group.members.iter().any(|m| m.window == f));
        let shallowest = group
            .members
            .iter()
            .filter_map(|m| within.get(&m.window))
            .copied()
            .min()
            .unwrap_or(0);
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
        focus_off_strip: focus_is_off_strip(&stacked, focus),
        lifted,
        within,
        group_order: strip.into_iter().map(|(key, _, _)| key).collect(),
    }
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
        self.started
            .is_some_and(|started| started.elapsed() > self.duration + LIFT_GRACE)
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

    /// Whether a focus refresh pass is due at `progress`; takes it when it is.
    fn take_refresh(&mut self, progress: f64) -> bool {
        let allowed = capture_work_allowed(self.phase(), CaptureKind::Refresh);
        self.refresh.take_pass(progress, allowed)
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
        if crate::animation::platform::window_snapshot::outgrows(
            tile.snapshot.coverage.covered,
            tile.to.size,
        ) {
            return TileState::Reveal { fits };
        }
        let resizing =
            crate::animation::platform::window_snapshot::is_a_resize(tile.from.size, tile.to.size);
        if self.refresh.is_target(window) {
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
    /// destination, for entrances too. See "The reservation fallback" in `src/animation/docs/animation-smoothness.md`.
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
        Some(if self.awaiting.is_empty() {
            Claimed::Released
        } else {
            Claimed::Held
        })
    }

    /// Takes the first picture of a reserved entrance after the flight started moving. Returns the
    /// banded tile and what is left of the flight for it to travel.
    fn admit(
        &mut self,
        window: WindowId,
        snapshot: &WindowSnapshot,
    ) -> Option<(OverlayTile, Duration)> {
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
/// See "A render of the wrong display" in `src/animation/docs/capture-overlay-research.md`.
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
    /// Points reserved at the top of the display for the bar. The overlay window starts below it,
    /// while its coordinates stay the full display's, so a captured desktop and the tiles keep their
    /// registration. Zero with the bar off.
    top_band: f64,
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
    /// Whether a bar under the overlay is pictured; see `Event::SetDisplay`.
    picture_bar: bool,
    /// Places the real windows once the overlay covers them. Supplied by the owner.
    place_frames: Option<PlaceFrames>,
    lend_snapshots: Option<LendSnapshots>,
    on_flight: Option<OnFlight>,
    flight_report: FlightReport,
    /// The stand-in's pixels and the scale they were drawn at, made once and redrawn only if the
    /// display's scale changes.
    placeholder_image: Option<(
        f64,
        objc2_core_foundation::CFRetained<objc2_core_graphics::CGImage>,
    )>,
    /// When the edge bounce and the move nudge playing now come home.
    out_and_backs: OutAndBacks,
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
            top_band: 0.0,
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
            picture_bar: true,
            place_frames: None,
            lend_snapshots: None,
            on_flight: None,
            flight_report: FlightReport::default(),
            placeholder_image: None,
            out_and_backs: OutAndBacks::default(),
        }
    }

    pub fn set_lend_snapshots(&mut self, lend: LendSnapshots) {
        self.lend_snapshots = Some(lend);
    }

    pub fn set_place_frames(&mut self, place: PlaceFrames) {
        self.place_frames = Some(place);
    }

    pub fn set_on_flight(&mut self, on_flight: OnFlight) {
        self.on_flight = Some(on_flight);
    }

    fn report_flight(&self, edge: Option<bool>) {
        if let (Some(flying), Some(on_flight)) = (edge, &self.on_flight) {
            on_flight(flying);
        }
    }

    pub async fn run(mut self) {
        while let Some((span, event)) = self.rx.recv().await {
            let _guard = span.enter();
            self.handle(event);
        }
    }

    fn handle(&mut self, event: Event) {
        match event {
            Event::SetDisplay {
                id,
                frame,
                scale,
                picture_bar,
                top_band,
            } => self.set_display(id, frame, scale, picture_bar, top_band),
            Event::Animate { windows, focus, duration } => self.start(windows, focus, duration),
            Event::AnimateSurface {
                windows,
                from_offset,
                to_offset,
                final_frames,
                focus,
                duration,
                nudge,
            } => self.start_surface(
                windows,
                from_offset,
                to_offset,
                final_frames,
                focus,
                duration,
                nudge,
            ),
            Event::Bounce {
                windows,
                overshoot,
                final_frames,
                focus,
                duration,
            } => self.start_bounce(windows, overshoot, final_frames, focus, duration),
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
            Event::LendSnapshots(windows) => self.lend_snapshots(windows),
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
            if self.cache.insert(window, snapshot.clone()) && snapshot.is_usable() {
                self.offer_mid_flight(window, &snapshot, false);
            }
        }
    }

    /// Offers a picture the cache just took to the running flight per `should_swap_mid_flight`. One
    /// the cache refused is never offered: a flight would draw what the next one will not.
    fn offer_mid_flight(&mut self, window: WindowId, snapshot: &WindowSnapshot, settled: bool) {
        let Some(running) = self.running.as_ref() else { return };
        let progress = running.progress_if_started();
        let state = running.tile_state(window, snapshot);
        let fresh = running.refresh.is_fresh(window, snapshot.taken);
        match should_swap_mid_flight(state, settled, fresh, progress) {
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
                if let Some(running) = self.running.as_mut()
                    && matches!(state, TileState::MovingRefreshTarget { .. })
                {
                    running.refresh.cut(window, snapshot.taken);
                }
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
            // that was resized keeps a usable picture" in `src/animation/docs/capture-overlay-research.md`.
            .filter(|target| {
                let cached = self.cache.usable(target.window);
                crate::animation::platform::window_snapshot::needs_capture(
                    cached.map(|snapshot| snapshot.coverage),
                    (target.size.width, target.size.height),
                ) || cached.is_some_and(|snapshot| {
                    crate::animation::platform::window_snapshot::picture_is_stale(
                        snapshot.taken.elapsed(),
                    )
                })
            })
            .collect();
        if wanted.is_empty() {
            return Vec::new();
        }
        debug!(
            count = wanted.len(),
            "warming snapshots for reactor-supplied windows"
        );
        let requested = wanted.iter().map(|target| target.window).collect();
        self.service.request(wanted);
        requested
    }

    /// Queues background captures for every visible window on the display. Cheap to repeat.
    /// Hand out what is already cached for `windows`, in the order asked.
    ///
    /// `usable` rather than `get`: the SkyLight route returns a sliver for exactly the off-screen and
    /// inactive-workspace windows a switcher exists to show — measured at 40x1081 for an off-strip
    /// window and 1x28 for one on a hidden workspace — and stretching a sliver across a row is worse
    /// than showing no picture.
    ///
    /// Age is deliberately NOT a reason to refuse one. A ten-minute-old picture of a window beats a
    /// grey box; staleness is a reason to warm, not to withhold.
    fn lend_snapshots(&self, windows: Vec<WindowId>) {
        let Some(lend) = &self.lend_snapshots else {
            return;
        };
        let held: Vec<(WindowId, WindowSnapshot)> = windows
            .into_iter()
            .filter_map(|window| self.cache.usable(window).cloned().map(|snap| (window, snap)))
            .collect();
        lend(held);
    }

    fn warm_cache(&mut self) {
        let Some((display_frame, _)) = self.display else {
            warn!("no display geometry yet; cannot warm the snapshot cache");
            return;
        };
        let windows =
            crate::windows::platform::window_server::visible_windows_on_display(display_frame);
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

    fn set_display(&mut self, id: u32, frame: CGRect, scale: f64, picture_bar: bool, top_band: f64) {
        if !picture_bar {
            self.pictures.bar = None;
        }
        self.picture_bar = picture_bar;
        let first = self.display.is_none();
        let changed = self.display != Some((frame, scale))
            || self.display_id != Some(id)
            || self.top_band != top_band;
        self.display = Some((frame, scale));
        self.display_id = Some(id);
        self.top_band = top_band;
        self.service.set_scale(scale);
        if let Some(overlay) = self.overlay.as_mut() {
            overlay.set_frame(frame, top_band, scale);
        }
        if first || changed {
            self.warm_cache();
            self.warm_desktop();
            // The desktop render in flight belongs to the display just left; window captures do not.
            self.service.invalidate_desktop();
            self.pictures.forget();
            self.arm_bar_refresh();
        }
    }

    /// Creates the overlay on first use and keeps it forever: creation is too slow to pay per
    /// animation. See "Toggle alpha, do not order the window in and out" in the research doc.
    fn ensure_overlay(&mut self) -> Option<&mut TileOverlay> {
        if self.overlay.is_none() {
            let (frame, scale) = self.display?;
            match TileOverlay::new(frame, self.top_band, scale, self.mtm) {
                Some(overlay) => self.overlay = Some(overlay),
                None => {
                    warn!("could not create the animation overlay; animations will be skipped");
                    return None;
                }
            }
        }
        self.overlay.as_mut()
    }

    /// Recaptures both ends of a focus change, framed where the window is wholly on a display and by
    /// the service where it is clipped, which a framed capture returns only a sliver of.
    /// See "Mid-flight passes" in `src/animation/docs/animation-smoothness.md`.
    fn refresh_destination_among(&mut self, tiles: &[(WindowId, WindowServerId, CGSize)]) {
        let current = self.running.as_ref().and_then(|running| running.focus);
        let windows: Vec<WindowId> = tiles.iter().map(|(w, _, _)| *w).collect();
        let wanted = refresh_targets(self.last_focus, current, &windows);
        if wanted.is_empty() {
            return;
        }
        let (wanted, requests) = refresh_requests(tiles, &wanted);
        if let Some(running) = self.running.as_mut() {
            running.refresh.ask(wanted, Instant::now());
        }
        let (framed, clipped): (Vec<SnapshotTarget>, Vec<SnapshotTarget>) = requests
            .into_iter()
            .partition(|target| is_wholly_on_a_display(target.server_id));
        debug!(
            framed = framed.len(),
            clipped = clipped.len(),
            "destination refresh requested"
        );
        self.capture_framed(
            "focus-refresh",
            framed.iter().map(|target| target.window).collect(),
            |window, snapshot| {
                snapshot.is_usable().then_some(Event::PictureReady {
                    window,
                    snapshot,
                    settled: true,
                })
            },
        );
        self.service.request(clipped);
    }

    /// Harvests hairlines for `windows`. Results come back as `DressingReady`, or as a whole
    /// `PictureReady` when the window has a blur the capture that just landed could not contain: the
    /// harvest is the moment a window is on screen and at rest, which is when that can be put back.
    fn harvest_dressings(&self, windows: Vec<WindowId>) {
        self.capture_framed("dressing-harvest", windows, |window, snapshot| {
            if snapshot.carries_blur && snapshot.is_usable() {
                Some(Event::PictureReady {
                    window,
                    snapshot,
                    settled: true,
                })
            } else {
                snapshot.dressing.map(|dressing| Event::DressingReady { window, dressing })
            }
        });
    }

    /// Framed captures of `windows` on a plain thread, each turned into what `event` sends back. Never
    /// on the service's completion queue, which must not make capture calls (see `snapshot_service`).
    fn capture_framed(
        &self,
        name: &str,
        windows: Vec<WindowId>,
        event: fn(WindowId, WindowSnapshot) -> Option<Event>,
    ) {
        if windows.is_empty() {
            return;
        }
        let tx = self.tx.clone();
        let scale = self.display.map(|(_, scale)| scale).unwrap_or(2.0);
        std::thread::Builder::new()
            .name(name.to_string())
            .spawn(move || {
                for window in windows {
                    let server_id = WindowServerId::from(window);
                    let Some(snapshot) = capture_via_framed_with_dressing(server_id, scale) else {
                        continue;
                    };
                    if let Some(event) = event(window, snapshot) {
                        _ = tx.send(event);
                    }
                }
            })
            .ok();
    }

    /// Takes a finished hairline harvest: onto the cached snapshot, and onto a tile in flight.
    fn dressing_ready(
        &mut self,
        window: WindowId,
        dressing: crate::animation::platform::edge_dressing::EdgeDressing,
    ) {
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
        if snapshot.dressing.is_some() {
            if let Some(running) = self.running.as_mut() {
                running.harvested.insert(window);
            }
        }
        if self.cache.insert(window, snapshot.clone()) {
            self.offer_mid_flight(window, &snapshot, settled);
        }
    }

    /// How much of the running flight is left, in wall-clock time.
    fn remaining_flight(&self) -> Option<Duration> {
        self.running.as_ref().map(RunningAnimation::remaining)
    }

    fn phase(&self) -> FlightPhase {
        self.running.as_ref().map_or(FlightPhase::Idle, RunningAnimation::phase)
    }

    /// Chases the first settled picture for a holding grow or entrance, one thread per window.
    /// See "A grow holds, then reveals" in `src/animation/docs/animation-smoothness.md`.
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
                        let Some(info) = crate::windows::platform::window_server::get_window(server_id) else {
                            continue;
                        };
                        let frame_fits = crate::animation::platform::window_snapshot::fits_frame(
                            (info.frame.size.width, info.frame.size.height),
                            (size.width, size.height),
                        );
                        if !frame_fits {
                            last_print = None;
                            continue;
                        }
                        let Some(snapshot) =
                            crate::animation::platform::window_snapshot::capture_via_framed_with_dressing(
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
        let Some(running) = self.running.as_mut() else {
            return false;
        };
        let Some(claimed) = running.claim(window, snapshot) else {
            return false;
        };
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
        let Some(running) = self.running.as_mut() else {
            return false;
        };
        let Some((tile, duration)) = running.admit(window, snapshot) else {
            return false;
        };
        running.plan.entrances.push((tile.window, tile.from, tile.to));
        let banding = band_plan(&running.plan, &running.tiles, running.focus);
        if let Some(overlay) = self.overlay.as_mut() {
            overlay.add_tile(&tile, plan::GroupKey::Loose, &banding, duration);
        }
        debug!(
            pid = window.pid,
            idx = window.idx.get(),
            "window entered mid-flight"
        );
        true
    }

    /// A stand-in for a window of `size`, never cached. `None` only if the pixels cannot be drawn.
    fn placeholder_for(&mut self, size: CGSize, scale: f64) -> Option<WindowSnapshot> {
        if self.placeholder_image.as_ref().is_none_or(|(at, _)| *at != scale) {
            let (image, _) = crate::animation::platform::window_snapshot::placeholder_image(scale)?;
            self.placeholder_image = Some((scale, image));
        }
        let (_, image) = self.placeholder_image.as_ref()?;
        Some(crate::animation::platform::window_snapshot::placeholder(
            size,
            image.clone(),
        ))
    }

    /// The snapshot to draw for one window: any usable cached picture, whatever its shape; a
    /// wrong-shaped one is drawn cropped. See "Resizes through the overlay" in the doc.
    fn snapshot_for(&mut self, request: &AnimationRequest) -> Option<WindowSnapshot> {
        self.cache.usable(request.window).cloned()
    }

    /// Tiles for the border windows tracing the animated windows; each anchor is the window's real
    /// frame and its tile. See "Window borders during animations" in the doc.
    fn companion_tiles(
        &mut self,
        display: CGRect,
        anchors: &[(CGRect, &OverlayTile)],
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
            self.deferred_warm
                .iter()
                .chain(self.after_flight.iter().flat_map(|a| a.targets.iter()))
                .map(|t| t.window),
        );
        let candidates: Vec<(WindowServerId, CGRect)> =
            crate::windows::platform::window_server::visible_windows_on_display(display)
                .into_iter()
                .filter(|(id, _)| !managed.contains(&id.as_u32()))
                .collect();
        let mut claimed: std::collections::HashSet<u32> = std::collections::HashSet::new();
        let mut tiles = Vec::new();
        let mut targets = Vec::new();
        for &(real, anchor) in anchors {
            let Some((server_id, frame)) = companion_of(real, &candidates, display) else {
                continue;
            };
            // One border traces one window; stacked twins share a frame.
            if !claimed.insert(server_id.as_u32()) {
                continue;
            }
            let window = synthetic_window_id(server_id);
            debug!(
                wsid = server_id.as_u32(),
                anchor = format!(
                    "{:.0},{:.0} {:.0}x{:.0}",
                    real.origin.x, real.origin.y, real.size.width, real.size.height
                ),
                "border companion matched"
            );
            targets.push(SnapshotTarget {
                window,
                server_id,
                size: frame.size,
            });
            match self.cache.usable(window).cloned() {
                Some(snapshot) => tiles.push(border_tile(anchor, real, (window, frame), snapshot)),
                None => needs_capture.push(SnapshotTarget {
                    window,
                    server_id,
                    size: frame.size,
                }),
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

        let depths = crate::windows::platform::window_server::front_to_back_depths();

        let any_resize = windows.iter().any(|request| {
            crate::animation::platform::window_snapshot::is_a_resize(
                request.from.size,
                request.to.size,
            )
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
            let path = tile_path(
                live_frame(request.server_id),
                request.from,
                request.to,
                request.floating,
                &others,
                index,
                display_frame,
            );
            let Some(TilePath { start, end, .. }) = path else {
                offscreen += 1;
                debug!(
                    wsid = request.server_id.as_u32(),
                    from = format!(
                        "{:.0},{:.0} {:.0}x{:.0}",
                        request.from.origin.x,
                        request.from.origin.y,
                        request.from.size.width,
                        request.from.size.height
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
            };
            let snapshot = self.snapshot_for(request);
            // Queued now so the next switch has pixels, even if this one does not.
            if snapshot.as_ref().is_none_or(|s| {
                s.source == crate::animation::platform::window_snapshot::SnapshotSource::SkyLight
                    && !s.is_usable()
            }) {
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
            // A known window off this display with no picture flies with a stand-in instead of
            // leaving a hole; see `stands_in`. It covers nothing, so the arm below holds for its
            // reveal like any grow, and the real picture replaces it.
            let snapshot = picture_or_stand_in(
                snapshot,
                || {
                    crate::windows::platform::window_server::get_window(request.server_id)
                        .map(|info| info.frame)
                },
                display_frame,
                || self.placeholder_for(request.to.size, scale),
            );
            match snapshot {
                Some(snapshot) => {
                    // A grow whose picture cannot cover the destination holds for the reveal.
                    if crate::animation::platform::window_snapshot::outgrows(
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
                        companion: None,
                        focused: focus == Some(request.window),
                    });
                    starts.push((request.window, start));
                    resolved.push((request.window, start, end, request.floating));
                }
                // No picture: almost always a window that just opened. See "A window that opens
                // travels from its spawn frame" in `src/animation/docs/animation-smoothness.md`.
                None => {
                    // Only a frame on this display counts as a spawn; capturing off screen is slow.
                    let spawn =
                        crate::windows::platform::window_server::get_window(request.server_id)
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
                                companion: None,
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
        let anchors: Vec<(CGRect, &OverlayTile)> = starts
            .iter()
            .filter_map(|(window, start)| {
                Some((*start, tiles.iter().find(|tile| tile.window == *window)?))
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
            None,
        );
    }

    /// Runs one plan through the shared machinery: merge into a running flight (`merge_plans`),
    /// or install a fresh one. See "The overlay engine" in `src/animation/docs/animation-smoothness.md`.
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
        nudge: Option<Nudge>,
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
                if !in_flight
                    && let Some(waiting) =
                        nudge_waiting(nudge, duration, &self.out_and_backs, Instant::now())
                {
                    running.nudge = Some(waiting);
                }
            }
            if in_flight {
                // See "Mid-flight passes" in `src/animation/docs/animation-smoothness.md`.
                let Self {
                    overlay,
                    running,
                    out_and_backs,
                    ..
                } = self;
                let running = running.as_mut().expect("checked above");
                let presented =
                    overlay.as_ref().map(|o| o.presented_positions()).unwrap_or_default();
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
                let now = Instant::now();
                let nudge = nudge.and_then(|nudge| {
                    let carrier = running.plan.nudge_carrier(nudge.window);
                    admitted(nudge, nudge_admission(out_and_backs, now, true, carrier))
                });
                // A flight carrying the nudge is stretched, its new legs with it.
                let duration = if nudge.is_some() {
                    duration.mul_f64(NUDGE_STRETCH)
                } else {
                    duration
                };
                if !delta.is_empty() {
                    debug!(
                        groups =
                            running.plan.groups.iter().filter(|g| !g.members.is_empty()).count(),
                        changing = running.plan.changing.len(),
                        entrances = running.plan.entrances.len(),
                        reparented = delta.reparented.len(),
                        retargeted_groups = delta.retargeted_groups.len(),
                        joined = delta.joined_tiles.len(),
                        "flight merged"
                    );
                    for (key, to) in &delta.retargeted_groups {
                        let presented = presented.drawn.get(key).copied().unwrap_or_default();
                        debug!(
                            key = format!("{key:?}"),
                            presented = format!("{:.0},{:.0}", presented.x, presented.y),
                            to = format!("{:.0},{:.0}", to.x, to.y),
                            "group retargeted"
                        );
                    }
                    for (window, from_key, to_key) in &delta.reparented {
                        let dest = running.plan.member(*window).map(|m| match m {
                            plan::Member::Rigid { key, rel } => {
                                plan::overlay_of(rel, running.plan.position_of(key))
                            }
                            plan::Member::Changing { to, .. }
                            | plan::Member::Entrance { to, .. } => to,
                            plan::Member::Floating { to, .. } => plan::overlay_of(
                                to,
                                running.plan.position_of(plan::GroupKey::Floating),
                            ),
                        });
                        let from_to = running.plan.position_of(*from_key);
                        debug!(
                            pid = window.pid,
                            idx = window.idx.get(),
                            from = format!("{from_key:?}"),
                            to = format!("{to_key:?}"),
                            group_destination = format!("{:.0},{:.0}", from_to.x, from_to.y),
                            member_destination = dest
                                .map(|d| format!(
                                    "{:.0},{:.0} {:.0}x{:.0}",
                                    d.origin.x, d.origin.y, d.size.width, d.size.height
                                ))
                                .unwrap_or_default(),
                            "member reparented"
                        );
                    }
                }
                let mut legs = Duration::ZERO;
                if let Some(overlay) = overlay.as_mut() {
                    let banding = band_plan(&running.plan, &running.tiles, running.focus);
                    legs =
                        overlay.retarget(&delta, &running.plan, &running.tiles, &banding, duration);
                }
                let changed = delta.moves_anything();
                if changed {
                    // The orchestration clock restarts so placement and teardown cover the new legs.
                    running.started = Some(Instant::now());
                    running.duration = duration.max(legs);
                }
                running.absorb_in_flight_change(changed, frames_changed);
                if let (Some((nudge, key)), Some(overlay)) = (nudge, overlay.as_mut()) {
                    nudge_on(overlay, running, out_and_backs, nudge, key, duration);
                }
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
        let edge = self.flight_report.overlay_up();
        self.report_flight(edge);

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
            refresh: FocusRefresh::default(),
            harvested: HashSet::new(),
            focus,
            plan,
            nudge: nudge_waiting(nudge, duration, &self.out_and_backs, Instant::now()),
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

    /// Animates the whole strip surface as one rigid group, one container and one position
    /// animation, with a container of its own for each column a move swapped, and the moved
    /// window's `nudge` riding its container. See "Strip movements" in
    /// `src/animation/docs/animation-smoothness.md`.
    fn start_surface(
        &mut self,
        windows: Vec<SurfaceWindow>,
        from_offset: CGPoint,
        to_offset: CGPoint,
        final_frames: Vec<(WindowId, CGRect)>,
        focus: Option<WindowId>,
        duration: Duration,
        nudge: Option<Nudge>,
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

        let depths = crate::windows::platform::window_server::front_to_back_depths();
        // One query for every window, not one each: a round trip per window stalled this thread
        // for 100-200ms a press. See "Rapid presses" in `specs/focus.md`.
        let real_frames: HashMap<WindowServerId, CGRect> =
            crate::windows::platform::window_server::get_windows(
                &windows.iter().map(|w| w.server_id).collect::<Vec<_>>(),
            )
            .into_iter()
            .map(|info| (info.id, info.frame))
            .collect();
        let mut tiles = Vec::with_capacity(windows.len());
        let mut missing = 0usize;
        let mut misshapen = 0usize;
        let mut stand_ins = 0usize;
        let mut needs_capture: Vec<SnapshotTarget> = Vec::new();
        let mut starts: Vec<(WindowId, CGRect)> = Vec::new();
        let display_frame = self.display.map(|(frame, _)| frame);
        let scale = self.display.map(|(_, scale)| scale).unwrap_or(2.0);
        for window in &windows {
            let (from, to) = window.travel(from_offset, to_offset);
            let cached = self.cache.usable(window.window).cloned();
            // A known window parked off this display with no picture flies as the stand-in, as on
            // the per-window path; see `stands_in`.
            let snapshot = match display_frame {
                Some(display) => picture_or_stand_in(
                    cached,
                    || real_frames.get(&window.server_id).copied(),
                    display,
                    || self.placeholder_for(window.frame.size, scale),
                ),
                None => cached,
            };
            match snapshot {
                Some(snapshot) => {
                    // A wrong-shaped picture is stretched rather than dropped. See "A window that
                    // was resized keeps a usable picture" in `src/animation/docs/capture-overlay-research.md`.
                    if snapshot.source
                        == crate::animation::platform::window_snapshot::SnapshotSource::Placeholder
                    {
                        stand_ins += 1;
                    } else if !snapshot.fits(window.frame.size) {
                        misshapen += 1;
                    }
                    if let Some(frame) = real_frames.get(&window.server_id) {
                        starts.push((window.window, *frame));
                    }
                    tiles.push(OverlayTile {
                        window: window.window,
                        from,
                        to,
                        snapshot,
                        floating: window.floating,
                        server_order: depths.get(&window.server_id.as_u32()).copied(),
                        depth: 0,
                        companion: None,
                        focused: focus == Some(window.window),
                    });
                }
                // Still placed by `final_frames`, and warmed once the movement settles.
                None => missing += 1,
            }
        }
        restack(&mut tiles, focus);
        let chase = display_frame
            .map(|display| stand_in_chase(&tiles, to_overlay_space(display, display)))
            .unwrap_or_default();
        let anchors: Vec<(CGRect, &OverlayTile)> = starts
            .iter()
            .filter_map(|(window, real)| {
                Some((*real, tiles.iter().find(|tile| tile.window == *window)?))
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
            stand_ins,
            travel = format!(
                "{:.0},{:.0} -> {:.0},{:.0}",
                from_offset.x, from_offset.y, to_offset.x, to_offset.y
            ),
            "surface group animation"
        );

        // One rigid piece for the strip and one for each column a move swapped; companions are
        // adopted by their own vectors.
        let drawn: Vec<SurfaceWindow> = windows
            .iter()
            .filter(|w| tiles.iter().any(|t| t.window == w.window && t.companion.is_none()))
            .cloned()
            .collect();
        let mut plan = plan::surface_plan(&drawn, from_offset, to_offset);
        for tile in &tiles {
            if plan.member(tile.window).is_none() {
                plan.adopt(tile);
            }
        }

        // A strip movement never resizes, never carries a brand-new window and never holds.
        self.begin_group(
            tiles,
            final_frames,
            duration,
            "surface",
            GroupStart::Immediate,
            apply_frames_at(FlightKind::Pan, false),
            Vec::new(),
            Vec::new(),
            chase,
            Vec::new(),
            focus,
            Some(pan_travel(from_offset, to_offset)),
            plan,
            nudge,
        );
    }

    /// Nudges the strip surface by `overshoot` and back, additively on a running flight or as the
    /// only motion of a no-travel one. See "Edge bounce" in `src/animation/docs/animation-smoothness.md`.
    fn start_bounce(
        &mut self,
        windows: Vec<SurfaceWindow>,
        overshoot: CGPoint,
        final_frames: Vec<(WindowId, CGRect)>,
        focus: Option<WindowId>,
        duration: Duration,
    ) {
        let now = Instant::now();
        if !self.out_and_backs.admits(OutAndBack::Bounce, now) {
            debug!("edge bounce skipped: one is still playing");
            return;
        }
        if self.running.is_none() {
            let at_rest = CGPoint::new(0.0, 0.0);
            self.start_surface(windows, at_rest, at_rest, final_frames, focus, duration, None);
        }
        let Self {
            overlay,
            running,
            out_and_backs,
            ..
        } = self;
        let (Some(overlay), Some(running)) = (overlay.as_mut(), running.as_mut()) else {
            return;
        };
        out_and_backs.start(OutAndBack::Bounce, now, duration);
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
        debug!(
            windows = running.tiles.len(),
            "starting the animation after coalescing"
        );
        let now = Instant::now();
        running.started = Some(now);
        let nudge = running.nudge.take().and_then(|(nudge, plain)| {
            let carrier = running.plan.nudge_carrier(nudge.window);
            let (nudge, key) =
                admitted(nudge, nudge_admission(&self.out_and_backs, now, true, carrier))?;
            Some((nudge, key, plain.mul_f64(NUDGE_STRETCH)))
        });
        // A flight carrying the nudge is stretched.
        if let Some((_, _, stretched)) = nudge {
            running.duration = running.duration.max(stretched);
        }
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
        if let Some((nudge, key, stretched)) = nudge
            && let (Some(overlay), Some(running)) = (self.overlay.as_mut(), self.running.as_mut())
        {
            nudge_on(overlay, running, &mut self.out_and_backs, nudge, key, stretched);
        }
    }

    fn step(&mut self) {
        self.trace_presented();
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
                debug!(
                    settled,
                    landed,
                    late_ms,
                    worst_pt = format!("{worst_pt:.2}"),
                    "flight landed"
                );
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
                        .filter(|tile| tile.companion.is_none())
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

    /// Where each container is drawn, every tick, under `rini::animation::trace` at trace level: the
    /// measurement behind "One flight however many presses" in
    /// `src/animation/docs/animation-smoothness.md`. Off, it reads nothing.
    fn trace_presented(&self) {
        if !tracing::enabled!(target: "rini::animation::trace", tracing::Level::TRACE) {
            return;
        }
        let Some(overlay) = self.overlay.as_ref() else { return };
        for (key, at) in overlay.presented_positions().drawn {
            tracing::trace!(
                target: "rini::animation::trace",
                key = format!("{key:?}"),
                x = format!("{:.1}", at.x),
                y = format!("{:.1}", at.y),
                "presented"
            );
        }
    }

    /// Logs how many real windows are not where their tiles finished, and the worst of them.
    /// See "Real windows land before lift" in `src/animation/docs/animation-smoothness.md`.
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
        let asked: Vec<WindowId> = running
            .final_frames
            .iter()
            .map(|(window, _)| *window)
            .filter(|window| tiled.contains(window))
            .collect();
        let by_server: HashMap<WindowServerId, CGRect> =
            crate::windows::platform::window_server::get_windows(
                &asked.iter().map(|w| WindowServerId::new(w.idx.get())).collect::<Vec<_>>(),
            )
            .into_iter()
            .map(|info| (info.id, info.frame))
            .collect();
        let real: HashMap<WindowId, CGRect> = asked
            .into_iter()
            .filter_map(|w| Some((w, *by_server.get(&WindowServerId::new(w.idx.get()))?)))
            .collect();
        Some(handover_report(
            &running.final_frames,
            &tiled,
            &real,
            display_frame,
        ))
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
        // render of the wrong display, drawn at its own size" in `src/animation/docs/capture-overlay-research.md`.
        if let Some(rendered) = self.pictures.desktop.clone().filter(|rendered| {
            crate::animation::platform::window_snapshot::spans_display(
                rendered.coverage.covered,
                display_size,
            )
        }) {
            self.pictures.drawn_once = true;
            return Some(rendered);
        }

        // No render yet: the synchronous composite covers the gap rather than leaving the overlay black.
        let desktop = crate::animation::platform::backdrop::desktop_backdrop_windows(display_frame);
        let composite = crate::animation::platform::window_snapshot::capture_composite_via_skylight(
            &desktop.windows,
            display_size,
            scale,
        );
        let usable = composite.filter(|snapshot| {
            crate::animation::platform::window_snapshot::is_backdrop_worth_drawing(
                self.pictures.drawn_once,
                desktop.has_wallpaper(),
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
                    has_wallpaper = desktop.has_wallpaper(),
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
                .map(|r| format!(
                    "{:.0},{:.0} {:.0}x{:.0}",
                    r.origin.x, r.origin.y, r.size.width, r.size.height
                ))
                .unwrap_or_else(|| "none".to_string()),
            "overlay dressed"
        );
    }

    /// The bar's held picture and where it sits in overlay coordinates. Only the very first call
    /// captures inline; [`Self::refresh_bar`] pays for the rest after an animation.
    fn bar_picture(&mut self) -> (Option<WindowSnapshot>, Option<CGRect>) {
        let Some((display_frame, _)) = self.display.filter(|_| self.picture_bar) else {
            return (None, None);
        };
        let strip = crate::animation::platform::backdrop::bar_strip(display_frame);
        let Some(bounds) = strip.bounds else {
            return (None, None);
        };
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
        if !self.picture_bar {
            return;
        }
        let tx = self.tx.clone();
        self.bar_refresh = RepeatingTimer::every(BAR_REFRESH_DELAY, move || {
            _ = tx.send(Event::RefreshBar);
        });
    }

    /// Recaptures the bar on its own, keeping its alpha; a no-op while the overlay covers it.
    /// See "The bar has to be captured on its own" in `src/animation/docs/capture-overlay-research.md`.
    fn refresh_bar(&mut self) {
        let Some((display_frame, scale)) = self.display else {
            return;
        };
        if self.overlay.as_ref().is_some_and(TileOverlay::is_visible) {
            return;
        }
        let strip = crate::animation::platform::backdrop::bar_strip(display_frame);
        let Some(bounds) = strip.bounds else { return };
        let fresh = crate::animation::platform::window_snapshot::capture_composite_via_skylight(
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
        let edge = self.flight_report.settled();
        self.report_flight(edge);
        let Some(after) = self.after_flight.take() else { return };
        let animated: Vec<WindowId> = after.targets.iter().map(|target| target.window).collect();
        let requested = if after.targets.is_empty() {
            Vec::new()
        } else {
            self.warm_windows(after.targets)
        };
        let dressed: HashSet<WindowId> = animated
            .iter()
            .copied()
            .filter(|window| self.cache.get(*window).is_some_and(|s| s.dressing.is_some()))
            .collect();
        self.harvest_dressings(finish_harvest_set(
            &animated,
            &after.harvested,
            &requested,
            &dressed,
        ));
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
        let windows =
            crate::windows::platform::window_server::visible_windows_on_display(display_frame);
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
/// The frame the window server reports for a window, if it reports a real one.
///
/// A zero-sized frame is macOS saying it has not laid the window out yet, which is not a position to
/// animate from. The geometry decision is `travel::tile_path`; this is the read it needs.
fn live_frame(server_id: WindowServerId) -> Option<CGRect> {
    crate::windows::platform::window_server::get_window(server_id)
        .map(|info| info.frame)
        .filter(|frame| frame.size.width > 0.0 && frame.size.height > 0.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::animation::domain::motion::surface::surface_travel;
    use crate::animation::domain::motion::travel::{
        neighbour_travel, resolve_end, resolve_start, travel_subject, worth_animating,
    };

    /// The built-in display, for tests that need a screen to judge parks against.
    const DISPLAY: CGRect = CGRect {
        origin: CGPoint { x: 0.0, y: 0.0 },
        size: CGSize { width: 1728.0, height: 1117.0 },
    };

    #[test]
    fn a_resize_places_the_real_windows_earlier() {
        assert!(
            apply_frames_at(FlightKind::Layout, true) < apply_frames_at(FlightKind::Layout, false)
        );
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
        assert!(
            !pictures.drawn_once,
            "a display we have never drawn has no backdrop to keep"
        );
    }

    mod refresh {
        use super::*;

        fn wid(idx: u32) -> WindowId {
            WindowId {
                pid: 1,
                idx: std::num::NonZeroU32::new(idx).unwrap(),
            }
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
            assert_eq!(
                refresh_targets(Some(wid(1)), Some(wid(2)), &tiles),
                vec![wid(2), wid(1)]
            );
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
            assert!(
                refresh_requests(&tiles, &pan).1.is_empty(),
                "nothing is captured"
            );

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
        use crate::animation::platform::window_snapshot::test_snapshot;

        fn wid(idx: u32) -> WindowId {
            WindowId {
                pid: 7,
                idx: std::num::NonZeroU32::new(idx).unwrap(),
            }
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
                companion: None,
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
                refresh: FocusRefresh::default(),
                harvested: HashSet::new(),
                focus: None,
                plan: plan::FlightPlan::empty(),
                nudge: None,
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
                vec![
                    Admitted::Redundant,
                    Admitted::Retargeted,
                    Admitted::Redundant
                ],
                "S1 confirmed, S2 retargeted, F confirmed"
            );

            let depth = |w: WindowId| flight.tiles.iter().find(|t| t.window == w).unwrap().depth;
            let (d1, d2, df) = (depth(s1), depth(s2), depth(f));
            assert!(
                !(d1 < df && df < d2) && !(d2 < df && df < d1),
                "S1={d1} S2={d2} F={df}: the floating window is between two strip tiles"
            );
            assert!(
                df < d1 && df < d2,
                "the floating focus leads, and its group with it"
            );
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
                (
                    "park the server already reports at its slot",
                    Some(slot),
                    park,
                    slot,
                ),
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
            assert!(
                wrong.is_empty(),
                "parks not remapped to the edge:\n{}",
                wrong.join("\n")
            );
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
            WindowId {
                pid: 7,
                idx: std::num::NonZeroU32::new(idx).unwrap(),
            }
        }

        /// T1 (1.1).
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

        /// T2 (1.1, 1.2).
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
            assert!(
                allowed.is_empty(),
                "capture work allowed in flight: {}",
                allowed.join(", ")
            );
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
                report.count_over == 1
                    && report.worst_visible_pt == 3.0
                    && report.worst_wsid == 200,
                "expected count_over=1 worst=3pt wsid=200; got {report:?}"
            );
        }

        /// T8 (1.4). See "A grow holds, then reveals" in `src/animation/docs/animation-smoothness.md`.
        #[test]
        fn a_hold_is_short_and_settles_on_the_first_repaint() {
            let limit = reveal_hold_limit(Duration::from_millis(300));
            let p = vec![10u8; 64];
            let q = vec![200u8; 64];
            let settled = chase_settled(None, &p, Some(&q));
            let wrong: Vec<String> = [
                (
                    limit <= HOLD_CAP,
                    format!("reveal_hold_limit(300ms) = {limit:?}"),
                ),
                (
                    REVEAL_CHASE_INTERVAL == Duration::from_millis(8),
                    format!("REVEAL_CHASE_INTERVAL = {REVEAL_CHASE_INTERVAL:?}"),
                ),
                (
                    settled,
                    format!("chase_settled(None, p, Some(q != p)) = {settled}"),
                ),
            ]
            .into_iter()
            .filter(|(ok, _)| !ok)
            .map(|(_, why)| why)
            .collect();
            assert!(
                wrong.is_empty(),
                "hold is not bounded and cheap:\n{}",
                wrong.join("\n")
            );
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
        use crate::animation::platform::window_snapshot::test_snapshot;

        fn wid(idx: u32) -> WindowId {
            WindowId {
                pid: 7,
                idx: std::num::NonZeroU32::new(idx).unwrap(),
            }
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
                refresh: FocusRefresh::default(),
                harvested: HashSet::new(),
                focus: None,
                plan: plan::FlightPlan::empty(),
                nudge: None,
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
                companion: None,
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

        /// 2.1.
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

        /// 2.1, 2.4. Seed 95, 200 runs.
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

        /// Seed 97, 200 runs.
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
        fn the_refresh_asks_once_for_each_wanted_window_with_a_tile() {
            let size = CGSize::new(859.0, 1081.0);
            let tiles = vec![
                (wid(1), WindowServerId::new(10), size),
                (wid(2), WindowServerId::new(20), size),
                (wid(3), WindowServerId::new(30), size),
            ];
            let (covered, requests) = refresh_requests(&tiles, &[wid(2), wid(1), wid(9)]);
            assert_eq!(
                covered,
                vec![wid(2), wid(1)],
                "an unknown window is not a target"
            );
            let asked: Vec<(WindowId, u32)> =
                requests.iter().map(|t| (t.window, t.server_id.as_u32())).collect();
            assert_eq!(asked, vec![(wid(2), 20), (wid(1), 10)], "one request per window");
            assert!(requests.iter().all(|t| t.size == size));
            assert!(refresh_requests(&tiles, &[]).1.is_empty());
        }

        /// 2.1, 3.6.
        #[test]
        fn two_refresh_passes_per_flight_and_none_at_frame_zero() {
            let mut holding = flight(None);
            holding.awaiting.push((wid(1), CGSize::new(859.0, 1081.0)));
            assert!(!holding.take_refresh(0.0), "a hold does not refresh");
            assert!(
                !holding.take_refresh(0.6),
                "nor does it take a pass it cannot use"
            );

            let mut running = flight(Some(Instant::now()));
            let fired: Vec<f64> = (0..=100)
                .map(|i| i as f64 / 100.0)
                .filter(|&progress| running.take_refresh(progress))
                .collect();
            assert_eq!(fired, vec![0.25, 0.55], "refresh passes taken: {fired:?}");
            assert!(
                !(0..=100).any(|i| running.take_refresh(i as f64 / 100.0)),
                "spent"
            );
            assert_eq!(REFRESH_PASSES_AT, [0.25, 0.55]);
            assert_eq!(REFRESH_APPLY_BEFORE, 0.9);
            assert_eq!(REVEAL_APPLY_BEFORE, 0.6);
        }

        /// A first tick late in the flight takes one pass, not every pass it slept through.
        #[test]
        fn a_late_first_look_takes_one_pass() {
            let mut running = flight(Some(Instant::now()));
            assert!(running.take_refresh(0.7));
            assert!(
                !running.take_refresh(0.71),
                "the overslept pass is not taken after it"
            );
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
            running.refresh.ask(vec![wid(3)], Instant::now());
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
            assert_eq!(
                running.tile_state(wid(4), &picture),
                TileState::Awaiting,
                "an entrance"
            );
            assert_eq!(running.tile_state(wid(5), &picture), TileState::NotTiled);

            // Once claimed, the grow is an ordinary resizing tile: a smaller picture no longer fits.
            running.awaiting.retain(|(w, _)| *w != wid(2));
            assert_eq!(
                running.tile_state(wid(2), &picture),
                TileState::Moving { fits: false, resizing: true }
            );
            // A refresh target in flight but past its hold is still only the refresh target.
            running.refresh.ask(vec![wid(3), wid(2)], Instant::now());
            assert_eq!(
                running.tile_state(wid(2), &test_snapshot(grown.size)),
                TileState::MovingRefreshTarget { fits: true, resizing: true }
            );
        }

        const PHASES: [FlightPhase; 4] = [
            FlightPhase::Idle,
            FlightPhase::FrameZero,
            FlightPhase::Holding,
            FlightPhase::Moving,
        ];
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
                    assert!(
                        capture_work_allowed(phase, kind),
                        "seed 96: idle refused {kind:?}"
                    );
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
            assert_eq!(
                deferred[0].size.width, 1147.0,
                "the later request replaces the earlier"
            );

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
            assert!(
                desktop_render_wanted(Some((fresh, (2560.0, 1440.0))), display),
                "misfit"
            );
            assert!(desktop_render_wanted(Some((stale, display)), display), "stale");
            assert!(
                !desktop_render_wanted(Some((fresh, display)), display),
                "in hand"
            );
        }

        /// 2.2.
        #[test]
        fn finish_harvests_each_animated_window_at_most_once() {
            let animated = [wid(1), wid(2), wid(3), wid(4), wid(2)];
            let mut harvested = HashSet::new();
            assert!(harvested.insert(wid(1)), "the chase's dressing");
            assert!(
                !harvested.insert(wid(1)),
                "a second harvest for the same window is skipped"
            );
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
            assert_eq!(
                APPLY_FRAMES_AT_PAN, 0.0,
                "a strip movement places its windows at frame zero"
            );
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
            let frames_changed = merge_final_frames(
                &mut running.final_frames,
                vec![(wid(1), slot), (wid(2), park_b)],
            );
            let outcomes = running.merge_pass(vec![tile(wid(1), slot, slot)], None);
            let changed = outcomes.iter().any(|(_, o)| *o != Admitted::Redundant);
            assert!(
                frames_changed && !changed,
                "the pass changes only the untiled frame"
            );
            running.absorb_in_flight_change(changed, frames_changed);
            assert!(
                !running.frames_applied,
                "an untiled change left frames_applied set"
            );

            // Nothing changes: the applied frames stand.
            running.frames_applied = true;
            let frames_changed = merge_final_frames(
                &mut running.final_frames,
                vec![(wid(1), slot), (wid(2), park_b)],
            );
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
                (
                    wid(108),
                    rect(1727.0, 1116.0, 859.0, 1081.0),
                    rect(1727.0, 1051.0, 859.0, 1081.0),
                ),
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
                (
                    wid(1),
                    rect(1728.0, 32.0, 1720.0, 1081.0),
                    rect(-1718.0, 32.0, 1720.0, 1081.0),
                ),
                (
                    wid(2),
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
                    wid(1),
                    rect(4.0, 32.0, 859.0, 1081.0),
                    rect(9.0, 32.0, 859.0, 1081.0),
                ),
                (wid(2), slot, rect(867.0, 40.0, 859.0, 1081.0)),
                (
                    wid(3),
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
                (wid(1), slot, slot),
                (
                    wid(2),
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
            assert!(
                chase_settled(None, &p, Some(&q)),
                "differs from the pre-resize picture"
            );
            assert!(!chase_settled(None, &p, Some(&p)), "still the old rendering");
            assert!(
                chase_settled(Some(&q), &p, Some(&q)),
                "a repaint settles even after a change"
            );
        }

        /// 2.4. See "A grow holds, then reveals" in `src/animation/docs/animation-smoothness.md`.
        #[test]
        fn the_hold_is_capped_at_a_blink() {
            for ms in [180u64, 300, 375, 500, 1000] {
                let d = Duration::from_millis(ms);
                assert_eq!(reveal_hold_limit(d), HOLD_CAP, "{ms}ms flight");
                assert!(reveal_hold_limit(d) <= reveal_hold_limit_old(d), "{ms}ms flight");
            }
            assert_eq!(HOLD_CAP, Duration::from_millis(300));
            assert_eq!(REVEAL_CHASE_INTERVAL, Duration::from_millis(8));
            assert_eq!(
                REVEAL_CHASE_INTERVAL * REVEAL_CHASE_ATTEMPTS as u32,
                Duration::from_secs(1)
            );
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
            assert_eq!(
                running.tile_state(wid(1), &reveal),
                TileState::Reveal { fits: true }
            );
            assert_eq!(
                running.tile_state(wid(1), &test_snapshot(small.size)),
                TileState::Reveal { fits: false },
                "a background picture at the old size is not the reveal"
            );
            let state = running.tile_state(wid(1), &reveal);
            assert_eq!(
                should_swap_mid_flight(state, true, false, Some(0.3)),
                SwapDecision::Swap("reveal"),
                "the chase's framed picture is the truth for a grow, refresh or not"
            );
            assert_eq!(
                should_swap_mid_flight(state, false, true, Some(0.3)),
                SwapDecision::CacheOnly,
                "an unsettled capture can be the unpainted surface"
            );
            assert_eq!(
                should_swap_mid_flight(state, true, true, Some(0.6)),
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
            assert_eq!(
                requested,
                Some(final_frames.clone()),
                "the slot went out with the rest"
            );
            assert!(running.frames_applied);
            running.started = Some(now);
            assert_eq!(
                running.frames_due(running.apply_at),
                None,
                "nothing left to send"
            );

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
            assert_eq!(
                running.claim(newcomer, &spawn),
                None,
                "a spawn-size picture is refused"
            );
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
        use crate::animation::platform::window_snapshot::{
            SnapshotCache, WindowSnapshot, needs_capture, outgrows, should_replace, test_snapshot,
        };

        fn wid(idx: u32) -> WindowId {
            WindowId {
                pid: 7,
                idx: std::num::NonZeroU32::new(idx).unwrap(),
            }
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
                refresh: FocusRefresh::default(),
                harvested: HashSet::new(),
                focus: None,
                plan: plan::FlightPlan::empty(),
                nudge: None,
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
                2 => TileState::Moving {
                    fits: rng.coin(),
                    resizing: rng.coin(),
                },
                _ => TileState::MovingRefreshTarget {
                    fits: rng.coin(),
                    resizing: rng.coin(),
                },
            }
        }

        /// The apply point before this spec, kept here for P-3.3.
        fn apply_frames_at_old(any_resize: bool) -> f64 {
            if any_resize {
                APPLY_FRAMES_AT_RESIZE
            } else {
                APPLY_FRAMES_AT
            }
        }

        /// P-3.2.
        #[test]
        fn an_awaited_picture_is_claimed_before_start_and_admitted_after() {
            let mut rng = Gen(92);
            for _ in 0..RUNS {
                let settled = rng.coin();
                let fresh = rng.coin();
                let progress = rng.below(1001) as f64 / 1000.0;
                assert_eq!(
                    should_swap_mid_flight(TileState::Awaiting, settled, fresh, None),
                    SwapDecision::Claim,
                    "seed 92: settled={settled} fresh={fresh}"
                );
                assert_eq!(
                    should_swap_mid_flight(TileState::Awaiting, settled, fresh, Some(progress)),
                    SwapDecision::Admit,
                    "seed 92: settled={settled} fresh={fresh} progress={progress}"
                );
            }
            let mut flight = flight(None);
            assert_eq!(flight.progress_if_started(), None, "holding: the claim path");
            flight.started = Some(Instant::now());
            assert!(flight.progress_if_started().is_some(), "moving: the admit path");
        }

        /// Outside the bug condition on the swap path.
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
            assert!(capture_work_allowed(
                FlightPhase::Idle,
                CaptureKind::NeedsCapture
            ));

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
                    let incoming = if rng.coin() {
                        test_snapshot(size)
                    } else {
                        clipped(size)
                    };
                    let before = cache.get(wid(1)).map(|s| s.coverage);
                    let progress = rng.coin().then(|| rng.below(1001) as f64 / 1000.0);
                    let decision = should_swap_mid_flight(
                        any_state(&mut rng),
                        rng.coin(),
                        rng.coin(),
                        progress,
                    );
                    decisions.push(decision);
                    let stored = cache.insert(wid(1), incoming.clone());
                    let held = cache.get(wid(1)).expect("seed 93: every landing leaves an entry");
                    assert_eq!(
                        stored,
                        should_replace(before, incoming.coverage),
                        "seed 93: insert reports what it did"
                    );
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
            assert!(
                refused > RUNS / 8,
                "generator sanity: {refused} downgrades refused"
            );
            let seen = |wanted: fn(&SwapDecision) -> bool| decisions.iter().any(wanted);
            assert!(seen(|d| *d == SwapDecision::Claim), "generator sanity: no Claim");
            assert!(seen(|d| *d == SwapDecision::Admit), "generator sanity: no Admit");
            assert!(
                seen(|d| matches!(d, SwapDecision::Swap(_))),
                "generator sanity: no Swap"
            );
            assert!(
                seen(|d| *d == SwapDecision::CacheOnly),
                "generator sanity: no CacheOnly"
            );
        }

        /// P-3.6. The preserved part: the refresh never fires at frame zero, and its passes are few.
        #[test]
        fn the_focus_refresh_fires_mid_flight_only() {
            let mut running = flight(Some(Instant::now()));
            let fired: Vec<f64> = (0..=100)
                .map(|i| i as f64 / 100.0)
                .filter(|&progress| running.take_refresh(progress))
                .collect();
            assert!(
                fired.iter().all(|p| *p > 0.0),
                "a refresh at frame zero: {fired:?}"
            );
            assert!(fired.len() <= 2, "more than the schedule allows: {fired:?}");
            assert!(
                refresh_targets(Some(wid(1)), Some(wid(2)), &[wid(1), wid(2), wid(3)]).len() <= 2
            );
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
                assert_eq!(
                    running.claim(wid(1), &snapshot),
                    None,
                    "seed 91: too small to claim"
                );
                assert_eq!(running.awaiting, awaiting, "seed 91: the hold stands");
                assert_eq!(
                    running.claim(wid(1), &test_snapshot(to.size)),
                    Some(Claimed::Released),
                    "seed 91: the reveal is claimed"
                );
                assert!(
                    running.tiles[0].snapshot.fits(to.size),
                    "seed 91: drawn from the reveal"
                );
                assert_eq!(
                    running.phase(),
                    FlightPhase::FrameZero,
                    "seed 91: released, not moving"
                );
            }
            assert_eq!(hold_wait(None, Instant::now()), None, "no deadline, no wait");
        }

        /// P-3.14.
        #[test]
        fn a_strip_window_on_this_display_with_no_usable_picture_is_placed_but_not_drawn() {
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

                let (from, to) =
                    surface_travel(slot, CGPoint::new(0.0, 0.0), CGPoint::new(861.0, 0.0), false);
                let mut running = flight(None);
                running.tiles.push(stacked(wid(3), from, to, Some(0), false));
                running.final_frames = vec![(wid(1), to), (wid(2), to), (wid(3), to)];
                let tiled: Vec<WindowId> = running.tiles.iter().map(|t| t.window).collect();
                let real: HashMap<WindowId, CGRect> =
                    running.final_frames.iter().copied().collect();
                let report = handover_report(&running.final_frames, &tiled, &real, DISPLAY);
                // A destination past the edge is a park, which the report excludes (2.3).
                let measured = usize::from(!rini_geometry::is_off_screen(DISPLAY, to));
                assert_eq!(
                    report.total, measured,
                    "seed 95: only the drawn window is measured"
                );
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
            assert_eq!(
                frame_zero_work(&[], &[], &[], &[]),
                (false, Vec::new(), Vec::new())
            );
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
                assert!(
                    running.awaiting.is_empty() && running.entrances.is_empty(),
                    "seed 96"
                );
                assert_eq!(running.phase(), FlightPhase::FrameZero, "seed 96");
                assert!(!running.frames_applied, "seed 96: nothing applied at frame zero");
                assert_eq!(hold_wait(running.hold_deadline, Instant::now()), None, "seed 96");
                assert_eq!(
                    running.claim(wid(1), &test_snapshot(CGSize::new(859.0, 1081.0))),
                    None
                );
            }
        }
    }

    /// Preservation for `.kiro/specs/exit-entrance-animation-regressions` bugfix.md 3.x: flights
    /// with no open or close.
    mod preservation {
        use super::*;
        use crate::animation::domain::motion::z_group::{GROUP_STRIDE, MAX_TILE_DEPTH};
        use crate::animation::platform::window_snapshot::{SnapshotCache, test_snapshot};

        pub(super) const DISPLAY: CGRect = CGRect {
            origin: CGPoint { x: 0.0, y: 0.0 },
            size: CGSize { width: 1728.0, height: 1117.0 },
        };
        pub(super) const RUNS: usize = 200;

        fn wid(idx: u32) -> WindowId {
            WindowId {
                pid: 7,
                idx: std::num::NonZeroU32::new(idx).unwrap(),
            }
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
                companion: None,
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
                refresh: FocusRefresh::default(),
                harvested: HashSet::new(),
                focus: None,
                plan: plan::FlightPlan::empty(),
                nudge: None,
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
            assert!(
                checked > RUNS / 2,
                "generator sanity: {checked} of {RUNS} in scope"
            );
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
                let expected = if current.same_as(incoming) {
                    Admitted::Redundant
                } else {
                    Admitted::Retargeted
                };
                assert_eq!(action, expected);
                let changed =
                    merge_final_frames(&mut flight.final_frames, vec![(wid(1), incoming)]);
                assert_eq!(changed, action == Admitted::Retargeted);
                assert_eq!(
                    flight.final_frames,
                    vec![(wid(1), incoming)],
                    "latest frame wins"
                );
                for in_flight in [false, true] {
                    assert_eq!(
                        reapply_set(false, in_flight, changed, &flight.final_frames),
                        None
                    );
                }
            }
            assert_eq!(
                merge_action(None, rect(4.0, 32.0, 859.0, 1081.0)),
                Admitted::Joined
            );
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
            assert!(
                checked > RUNS / 2,
                "generator sanity: {checked} of {RUNS} in scope"
            );
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
                    let band = if in_front {
                        0..GROUP_STRIDE
                    } else {
                        GROUP_STRIDE..2 * GROUP_STRIDE
                    };
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
            // The back of the band behind the strip. With focus on the strip there is no lifted band
            // in front, so that is two strides deep rather than the three `MAX_TILE_DEPTH` allows.
            assert_eq!(tiles[1].depth, 2 * GROUP_STRIDE - 1);
            assert!(tiles[1].depth <= MAX_TILE_DEPTH);
        }

        /// P-3.9.
        #[test]
        fn a_parked_or_scrolled_off_close_is_not_worth_animating() {
            let mut rng = Gen(39);
            for _ in 0..RUNS {
                let size = CGSize::new(rng.pt(400.0, 1720.0), 1081.0);
                let park = rng.park(size);
                assert!(
                    !worth_animating(park, entrance_from(park), DISPLAY),
                    "park {park:?}"
                );
                let off = rect(
                    DISPLAY.size.width + rng.pt(1.0, 9000.0),
                    32.0,
                    size.width,
                    size.height,
                );
                assert!(
                    !worth_animating(off, entrance_from(off), DISPLAY),
                    "off strip {off:?}"
                );
                let on = rng.on_screen();
                assert!(
                    worth_animating(on, entrance_from(on), DISPLAY),
                    "on screen {on:?}"
                );
            }
            let slot = rect(4.0, 32.0, 859.0, 1081.0);
            assert!(
                !worth_animating(rect(4.0, 32.0, 0.0, 0.0), slot, DISPLAY),
                "zero area"
            );
            // Entering from a park is still worth it: the path crosses the display.
            assert!(worth_animating(
                rect(1727.0, 1116.0, 859.0, 1081.0),
                slot,
                DISPLAY
            ));
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
                assert!(
                    chase.contains(&(wid(3), to.size)),
                    "the entrance is chased: {chase:?}"
                );

                let awaiting = vec![(wid(1), rng.on_screen().size)];
                let (apply_now, chase, _) = frame_zero_work(&awaiting, &[], &[], &[]);
                assert!(apply_now);
                assert_eq!(chase, awaiting);
            }
            assert_eq!(
                frame_zero_work(&[], &[], &[], &[]),
                (false, Vec::new(), Vec::new())
            );
        }
    }

    /// Change 3 of `.kiro/specs/exit-entrance-animation-regressions`: depth is banded once per flight
    /// from its latest focus. See "Mid-flight passes" in `src/animation/docs/animation-smoothness.md`.
    mod flight_restack {
        use super::preservation::{Gen, RUNS, stacked};
        use super::*;
        use crate::animation::domain::motion::z_group::{GROUP_STRIDE, MAX_TILE_DEPTH};

        fn wid(idx: u32) -> WindowId {
            WindowId {
                pid: 7,
                idx: std::num::NonZeroU32::new(idx).unwrap(),
            }
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
                refresh: FocusRefresh::default(),
                harvested: HashSet::new(),
                focus: None,
                plan: plan::FlightPlan::empty(),
                nudge: None,
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
            assert_eq!(
                depth(&flight, s1),
                1,
                "pass 1: the strip leads, server order within"
            );
            assert_eq!(depth(&flight, s2), 3);
            assert_eq!(
                depth(&flight, f),
                GROUP_STRIDE + 2,
                "the floating window behind the strip"
            );

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
            assert_eq!(
                depth(&flight, s1),
                GROUP_STRIDE + 1,
                "redundant tile: rebanded anyway"
            );
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
            assert_eq!(
                depth(&flight, wid(1)),
                GROUP_STRIDE + 2,
                "the floating focus still leads"
            );
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
            assert_eq!(
                depth(&flight, wid(3)),
                GROUP_STRIDE + 3,
                "redundant: old order kept"
            );
            assert_eq!(
                depth(&flight, wid(1)),
                GROUP_STRIDE + 5,
                "retargeted: the new order"
            );
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
            assert_eq!(
                depth(&flight, wid(5)),
                GROUP_STRIDE + 1,
                "the entrance leads the strip"
            );
            assert_eq!(depth(&flight, wid(1)), GROUP_STRIDE + 4);
        }

        /// Companions ride their window's depth and are not restacked on their own.
        #[test]
        fn a_companion_keeps_the_depth_it_was_given() {
            let slot = rect(4.0, 32.0, 859.0, 1081.0);
            let mut companion = stacked(wid(8), slot, slot, None, false);
            companion.companion = Some(wid(1));
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
        use crate::animation::platform::window_snapshot::test_snapshot;

        fn wid(idx: u32) -> WindowId {
            WindowId {
                pid: 7,
                idx: std::num::NonZeroU32::new(idx).unwrap(),
            }
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
                refresh: FocusRefresh::default(),
                harvested: HashSet::new(),
                focus: None,
                plan: plan::FlightPlan::empty(),
                nudge: None,
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
                let (entrance, waiting) = entrance_reservation(wid(2), to, floating);
                assert_eq!(waiting, Some((wid(2), to.size)), "seed 61: {to:?}");
                assert_eq!(entrance.to, to);
                assert_eq!(entrance.floating, floating);
            }
        }

        /// A parked window with no picture flies as a stand-in tile — no hole — held for like a grow,
        /// and its real picture replaces the stand-in on the SAME tile when it lands at frame zero.
        #[test]
        fn a_stand_in_takes_its_real_picture_on_the_same_tile() {
            use crate::animation::platform::window_snapshot::{placeholder, test_bitmap};
            let parked = rect(-1720.0, 32.0, 859.0, 1081.0);
            let slot = rect(4.0, 32.0, 859.0, 1081.0);
            let mut flight = flight(None);
            let mut tile = stacked(wid(3), parked, slot, Some(2), false);
            tile.snapshot = placeholder(slot.size, test_bitmap());
            flight.tiles.push(tile);
            flight.awaiting.push((wid(3), slot.size));

            assert!(
                matches!(
                    flight.tile_state(wid(3), &test_snapshot(slot.size)),
                    TileState::Reveal { fits: true }
                ),
                "held for like a grow"
            );
            let claimed = flight.claim(wid(3), &test_snapshot(slot.size));

            assert_eq!(claimed, Some(Claimed::Released));
            let tile = flight.tiles.iter().find(|t| t.window == wid(3)).expect("still one tile");
            assert_ne!(
                tile.snapshot.source,
                crate::animation::platform::window_snapshot::SnapshotSource::Placeholder,
                "the stand-in is gone"
            );
            assert_eq!((tile.from, tile.to), (parked, slot), "and it travels as it did");
            assert!(
                flight.entrances.is_empty(),
                "never a newcomer's zero-width entrance"
            );
        }

        /// Once the flight is moving without the picture, a stand-in still counts as a picture waiting
        /// for its reveal, so a settled capture landing early enough replaces it rather than waiting
        /// for the next flight.
        #[test]
        fn a_stand_in_mid_flight_is_swapped_for_its_picture() {
            use crate::animation::platform::window_snapshot::{placeholder, test_bitmap};
            let slot = rect(4.0, 32.0, 859.0, 1081.0);
            let mut flight = flight(Some(Instant::now()));
            let mut tile = stacked(wid(3), slot, slot, Some(2), false);
            tile.snapshot = placeholder(slot.size, test_bitmap());
            flight.tiles.push(tile);

            let state = flight.tile_state(wid(3), &test_snapshot(slot.size));
            assert_eq!(state, TileState::Reveal { fits: true });
            assert_eq!(
                should_swap_mid_flight(state, true, false, Some(0.3)),
                SwapDecision::Swap("reveal")
            );
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
            assert_eq!(
                other.depth, 2,
                "its neighbour keeps the server's order, within the band"
            );
        }

        #[test]
        fn the_last_claim_releases_the_flight_and_a_repeat_is_not_a_hold() {
            let slot = rect(867.0, 32.0, 859.0, 1081.0);
            let mut flight = holding_for(wid(2), slot, true);
            assert_eq!(
                flight.claim(wid(2), &test_snapshot(slot.size)),
                Some(Claimed::Released)
            );
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
            assert_eq!(
                flight.claim(wid(2), &test_snapshot(CGSize::new(200.0, 200.0))),
                None
            );
            assert_eq!(flight.awaiting.len(), 1, "the hold goes on");
            assert_eq!(flight.entrances.len(), 1);
            assert!(
                flight.tiles.is_empty(),
                "nothing composed from the small picture"
            );

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
            assert_eq!(
                flight.claim(wid(1), &test_snapshot(big.size)),
                Some(Claimed::Released)
            );
            assert!(flight.tiles[0].snapshot.fits(big.size));
            assert_eq!(flight.tiles.len(), 1);
        }

        /// Before the flight starts, `admit` does nothing and leaves the reservation to `claim`.
        #[test]
        fn a_late_entrance_travels_for_the_remaining_flight() {
            let slot = rect(867.0, 32.0, 859.0, 1081.0);
            let mut flight = holding_for(wid(2), slot, false);
            assert!(
                flight.admit(wid(2), &test_snapshot(slot.size)).is_none(),
                "still holding"
            );
            assert_eq!(flight.entrances.len(), 1);

            // The deadline passed: `start_moving` flew with the placeholder and cleared the hold.
            flight.awaiting.clear();
            flight.started = Some(Instant::now() - flight.duration.mul_f64(0.6));
            let remaining = flight.remaining();
            let (tile, travel) = flight.admit(wid(2), &test_snapshot(slot.size)).expect("admitted");
            assert_eq!(tile.from, entrance_from(slot));
            assert_eq!(tile.to, slot);
            assert!(travel <= remaining, "{travel:?} outlives {remaining:?}");
            assert!(
                travel < flight.duration.mul_f64(0.5),
                "not the full duration: {travel:?}"
            );
            assert!(flight.entrances.is_empty());
            assert!(
                flight.admit(wid(2), &test_snapshot(slot.size)).is_none(),
                "taken once"
            );
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
            assert_eq!(
                late_join_duration(Duration::from_millis(300), 1.0),
                Duration::ZERO
            );
            assert_eq!(
                late_join_duration(Duration::from_millis(300), 1.5),
                Duration::ZERO
            );
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
                                if let Some(&(_, current)) =
                                    expected.iter().find(|(w, _)| *w == wid(i))
                                {
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
                    let reapply = reapply_set(
                        flight.frames_applied,
                        flight.started.is_some(),
                        changed,
                        &flight.final_frames,
                    );
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
    }

    /// Fix checking for Change 6 (bugfix.md 1.8, 2.8, 2.10): a pass flies only when something
    /// drawable moves or a flight is running.
    mod still_passes {
        use super::preservation::{Gen, RUNS, stacked};
        use super::*;

        fn wid(idx: u32) -> WindowId {
            WindowId {
                pid: 7,
                idx: std::num::NonZeroU32::new(idx).unwrap(),
            }
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
            assert!(
                waiting.is_some(),
                "the entrance is reserved but never drawn here"
            );
            assert!(!worth_flying(moving_drawable(&tiles), false));
        }

        #[test]
        fn a_strip_open_that_moves_a_neighbour_flies() {
            let before = rect(0.0, 32.0, 1720.0, 1081.0);
            let after = rect(0.0, 32.0, 860.0, 1081.0);
            let tiles = vec![stacked(wid(1), before, after, Some(1), false)];
            let (_, waiting) =
                entrance_reservation(wid(2), rect(867.0, 32.0, 859.0, 1081.0), false);
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
                        rect(
                            from.origin.x + rng.pt(1.0, 400.0),
                            32.0,
                            from.size.width,
                            from.size.height,
                        )
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
        assert_eq!(
            to_overlay_space(window, overlay),
            rect(865.0, 0.0, 859.0, 1081.0)
        );
    }

    #[test]
    fn a_bounce_extends_the_clock_to_cover_its_return() {
        let bounce = Duration::from_millis(350);
        assert_eq!(
            clock_for_bounce(None, Duration::from_millis(100), bounce),
            bounce
        );
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
        assert!(
            !lift_now(false, true, true, true),
            "overdue is meaningless before the clock is done"
        );
        assert!(
            !lift_now(true, false, true, false),
            "clock done, render server a frame behind: wait"
        );
        assert!(
            !lift_now(true, true, false, false),
            "clock done, a real window still travelling: wait"
        );
        assert!(lift_now(true, true, true, false));
        assert!(
            lift_now(true, false, false, true),
            "the grace ran out: lift anyway"
        );
        assert!(
            LIFT_GRACE < Duration::from_millis(500),
            "a stall is a hold, not a hang"
        );
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
            refresh: FocusRefresh::default(),
            harvested: HashSet::new(),
            focus: None,
            plan: plan::FlightPlan::empty(),
            nudge: None,
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
            refresh: FocusRefresh::default(),
            harvested: HashSet::new(),
            focus: None,
            plan: plan::FlightPlan::empty(),
            nudge: None,
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
            refresh: FocusRefresh::default(),
            harvested: HashSet::new(),
            focus: None,
            plan: plan::FlightPlan::empty(),
            nudge: None,
            _clock: None,
        };
        // Clamped, or the easing overshoots when a frame arrives late.
        assert_eq!(finished.progress(), 1.0);
        assert!(finished.is_done());
    }

    /// A pass merging into a flight in progress (`merge_plans`). The 3:27:20 tear is the case it
    /// exists for. See "Mid-flight passes" in `src/animation/docs/animation-smoothness.md`.
    mod rigid_strip {
        use super::preservation::{DISPLAY, Gen, RUNS};
        use super::rigid_groups::random_requests;
        use super::*;
        use crate::animation::platform::engine::plan::*;

        fn wid(idx: u32) -> WindowId {
            WindowId {
                pid: 7,
                idx: std::num::NonZeroU32::new(idx).unwrap(),
            }
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
                        key => plan
                            .groups
                            .iter()
                            .find(|g| g.key == *key)
                            .map(|g| g.travel)
                            .unwrap_or(CGPoint::new(0.0, 0.0)),
                    };
                    (*key, CGPoint::new(p.x - travel.x / 2.0, p.y - travel.y / 2.0))
                })
                .collect()
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
                entrance_reservation(wid(51462), to_overlay_space(slot, EXTERNAL), false);
            let (untouched, _) = entrance_reservation(
                wid(51463),
                to_overlay_space(rect(2000.0, 32.0, 400.0, 1081.0), EXTERNAL),
                false,
            );
            let mut entrances = vec![newcomer, untouched.clone()];
            let frames = vec![
                (wid(1), rect(4.0, 32.0, 859.0, 1081.0)),
                (wid(51462), shifted(slot, pan)),
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
                    SurfaceWindow {
                        window: wid(1),
                        server_id: WindowServerId::new(1),
                        frame: a,
                        pinned: false,
                        floating: false,
                        from: None,
                    },
                    SurfaceWindow {
                        window: wid(2),
                        server_id: WindowServerId::new(2),
                        frame: shifted(b, CGPoint::new(859.0, 0.0)),
                        pinned: false,
                        floating: false,
                        from: None,
                    },
                ],
                CGPoint::new(-574.0, 0.0),
                CGPoint::new(0.0, 0.0),
            );
            let (merged, delta) = merge_plans(
                &current,
                &pan,
                Some(d),
                &at_model(&current).into(),
                None,
                DISPLAY,
            );

            for group in before.groups.iter().filter(|g| !g.members.is_empty()) {
                let p = merged.position_of(group.key);
                assert_eq!(
                    p,
                    CGPoint::new(
                        before.position_of(group.key).x + d.x,
                        before.position_of(group.key).y + d.y
                    )
                );
                assert!(
                    delta.retargeted_groups.contains(&(group.key, p)),
                    "{:?} retargeted",
                    group.key
                );
                let after = merged.groups.iter().find(|g| g.key == group.key).unwrap();
                assert_eq!(after.members, group.members, "membership and rel untouched");
            }
            assert!(delta.reparented.is_empty());
            assert!(delta.new_groups.is_empty(), "the pan names known windows only");
            assert_eq!(merged.entrances[0].2, shifted(slot, d));
            assert_eq!(delta.retargeted_tiles, vec![(wid(9), shifted(slot, d))]);
            assert_eq!(
                dest(&merged, wid(3)),
                Some(shifted(c, CGPoint::new(859.0 + d.x, 0.0))),
                "a member the pan did not compose rides its group"
            );
        }

        #[test]
        fn a_redundant_pass_is_an_empty_delta() {
            let (a, b) = (column(0.0), column(1.0));
            let requests = [
                (wid(1), a, a, false),
                (wid(2), b, shifted(b, CGPoint::new(-300.0, 0.0)), false),
            ];
            let current = flight_of(&requests);
            let again = reflow_plan(&requests, DISPLAY);
            let (merged, delta) =
                merge_plans(&current, &again, None, &midway(&current).into(), None, DISPLAY);
            assert!(delta.is_empty(), "{delta:?}");
            assert_eq!(merged, current);
        }

        #[test]
        fn a_pass_moving_one_member_elsewhere_reparents_it_and_keeps_the_other() {
            let (a, b) = (column(0.0), column(1.0));
            let v = CGPoint::new(-300.0, 0.0);
            let current = flight_of(&[
                (wid(1), a, shifted(a, v), false),
                (wid(2), b, shifted(b, v), false),
            ]);
            let group = current.groups[1].key;
            let presented = midway(&current);
            let elsewhere = shifted(b, CGPoint::new(500.0, 0.0));
            let pass = reflow_plan(
                &[
                    (wid(1), a, shifted(a, v), false),
                    (wid(2), b, elsewhere, false),
                ],
                DISPLAY,
            );
            let (merged, delta) =
                merge_plans(&current, &pass, None, &presented.clone().into(), None, DISPLAY);
            assert_eq!(delta.reparented.len(), 1);
            let (window, from, to) = delta.reparented[0];
            assert_eq!((window, from), (wid(2), group));
            assert_ne!(to, group);
            assert_eq!(
                key_of(&merged, wid(1)),
                Some(group),
                "the other member keeps the container"
            );
            assert!(
                delta.retargeted_groups.is_empty(),
                "the winning cluster confirmed the destination"
            );
            assert!(
                dest(&merged, wid(2)).unwrap().same_as(elsewhere),
                "{:?}",
                dest(&merged, wid(2))
            );
            assert!(dest(&merged, wid(1)).unwrap().same_as(shifted(a, v)));
            assert_eq!(delta.new_groups.len(), 1, "no group had that remaining travel");
            assert_eq!(
                delta.new_groups[0].install, presented[&group],
                "installs where the old container is drawn"
            );
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
            let past_right =
                rect(DISPLAY.size.width + 1.0, b.origin.y, b.size.width, b.size.height);
            let past_left = rect(-c.size.width - 1.0, c.origin.y, c.size.width, c.size.height);
            let pass = reflow_plan(
                &[
                    (wid(1), a, shifted(a, up), false),
                    (wid(2), b, past_right, false),
                    (wid(3), c, past_left, false),
                ],
                DISPLAY,
            );
            let (merged, delta) =
                merge_plans(&current, &pass, None, &midway(&current).into(), None, DISPLAY);
            assert!(delta.is_empty(), "{delta:?}");
            assert_eq!(merged, current);
            for w in [wid(1), wid(2), wid(3)] {
                assert_eq!(key_of(&merged, w), Some(group));
            }

            // A still container lends no motion: the member leaves on its own vector.
            let still = flight_of(&[(wid(1), a, a, false), (wid(2), b, b, false)]);
            let pass =
                reflow_plan(&[(wid(1), a, a, false), (wid(2), b, past_right, false)], DISPLAY);
            let (merged, delta) =
                merge_plans(&still, &pass, None, &midway(&still).into(), None, DISPLAY);
            assert_eq!(delta.reparented.len(), 1, "{delta:?}");
            assert!(dest(&merged, wid(2)).unwrap().same_as(past_right));
        }

        #[test]
        fn a_rigid_member_turning_into_a_resize_goes_loose() {
            let (a, b) = (column(0.0), column(1.0));
            let v = CGPoint::new(-300.0, 0.0);
            let current = flight_of(&[
                (wid(1), a, shifted(a, v), false),
                (wid(2), b, shifted(b, v), false),
            ]);
            let group = current.groups[1].key;
            let presented = midway(&current);
            let grown = rect(b.origin.x + v.x, b.origin.y, b.size.width + 400.0, b.size.height);
            let pass = reflow_plan(&[(wid(2), shifted(b, v), grown, false)], DISPLAY);
            let (merged, delta) =
                merge_plans(&current, &pass, None, &presented.clone().into(), None, DISPLAY);
            assert_eq!(delta.reparented, vec![(wid(2), group, GroupKey::Loose)]);
            assert_eq!(delta.retargeted_tiles, vec![(wid(2), grown)]);
            let Some(Member::Changing { from, to }) = merged.member(wid(2)) else {
                panic!("loose")
            };
            assert_eq!(
                from,
                overlay_of(b, presented[&group]),
                "leaves at the presented frame"
            );
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
            let (merged, delta) =
                merge_plans(&current, &pass, None, &presented.clone().into(), None, DISPLAY);
            assert_eq!(delta.joined_tiles, vec![(wid(2), group)]);
            assert!(delta.new_groups.is_empty());
            let Some(Member::Rigid { key, rel }) = merged.member(wid(2)) else {
                panic!("rigid")
            };
            assert_eq!(key, group);
            assert_eq!(rel, group_relative(b, presented[&group]));
            assert!(dest(&merged, wid(2)).unwrap().same_as(shifted(b, remaining)));
        }

        #[test]
        fn a_join_with_a_new_vector_opens_a_group() {
            let (a, b) = (column(0.0), column(1.0));
            let current = flight_of(&[(wid(1), a, shifted(a, CGPoint::new(-400.0, 0.0)), false)]);
            let pass = reflow_plan(
                &[(wid(2), b, shifted(b, CGPoint::new(120.0, 0.0)), false)],
                DISPLAY,
            );
            let (merged, delta) =
                merge_plans(&current, &pass, None, &midway(&current).into(), None, DISPLAY);
            assert!(delta.joined_tiles.is_empty());
            assert_eq!(delta.new_groups.len(), 1);
            let plan::NewGroup { key, install, .. } = delta.new_groups[0];
            assert_eq!(install, CGPoint::new(0.0, 0.0));
            assert_eq!(key_of(&merged, wid(2)), Some(key));
            assert_eq!(merged.position_of(key), CGPoint::new(120.0, 0.0));
            assert_eq!(merged.next_key, current.next_key + 1);
        }

        /// The 3:27:20 case: an open with 22 survivors and one entrance, then a 574pt pan 56ms later.
        #[test]
        fn the_3_27_20_open_then_pan_ends_everything_at_the_pans_frames() {
            let col = |i: f64| {
                rect(
                    EXTERNAL.origin.x + 4.0 + i * 863.0,
                    EXTERNAL.origin.y + 32.0,
                    859.0,
                    1081.0,
                )
            };
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
                    from: None,
                })
                .collect();
            let pan =
                plan::surface_plan(&windows, CGPoint::new(-574.0, 0.0), CGPoint::new(0.0, 0.0));
            let (merged, delta) =
                merge_plans(&current, &pan, Some(d), &midway(&current).into(), None, DISPLAY);

            for (w, _, to, _) in &requests {
                let expected = shifted(to_overlay_space(*to, EXTERNAL), d);
                assert!(
                    dest(&merged, *w).unwrap().same_as(expected),
                    "{w:?}: {:?} vs {expected:?}",
                    dest(&merged, *w)
                );
            }
            assert_eq!(merged.entrances[0].2, shifted(slot_o, d));
            assert!(delta.reparented.is_empty());
            assert_eq!(
                delta.retargeted_groups.len(),
                2,
                "the still group and the pushed group"
            );
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
            let pass = reflow_plan(
                &[
                    (wid(1), a, shifted(a, further), false),
                    (wid(3), c, shifted(c, further), false),
                ],
                DISPLAY,
            );
            let (merged, delta) =
                merge_plans(&current, &pass, None, &midway(&current).into(), None, DISPLAY);
            assert_eq!(delta.retargeted_groups, vec![(group, further)]);
            assert!(delta.reparented.is_empty() && delta.joined_tiles.is_empty());
            assert!(
                dest(&merged, wid(2)).unwrap().same_as(shifted(b, further)),
                "rides along"
            );
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
                    let presented = if rng.coin() {
                        at_model(&plan)
                    } else {
                        midway(&plan)
                    };
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
                                        p.adopt(&super::preservation::stacked(
                                            *w,
                                            from,
                                            shifted(from, d),
                                            None,
                                            false,
                                        ));
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
                                let Some(Member::Rigid { key, rel }) = plan.member(*w) else {
                                    continue;
                                };
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
                            let rigid: Vec<WindowId> = windows
                                .iter()
                                .copied()
                                .filter(|w| matches!(plan.member(*w), Some(Member::Rigid { .. })))
                                .collect();
                            let mut reqs: Vec<(WindowId, CGRect, CGRect, bool)> = Vec::new();
                            if let Some(w) = rigid.first() {
                                let Some(Member::Rigid { key, rel }) = plan.member(*w) else {
                                    unreachable!()
                                };
                                let from = overlay_of(rel, presented[&key]);
                                let to = rect(
                                    from.origin.x,
                                    from.origin.y,
                                    from.size.width + 300.0,
                                    from.size.height,
                                );
                                reqs.push((*w, from, to, false));
                            }
                            reflow_plan(&reqs, DISPLAY)
                        }
                    };
                    let (merged, delta) = merge_plans(
                        &plan,
                        &incoming,
                        pan,
                        &presented.clone().into(),
                        None,
                        DISPLAY,
                    );

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
                            Member::Rigid { key, rel } => overlay_of(
                                rel,
                                incoming.groups.iter().find(|g| g.key == key).unwrap().travel,
                            ),
                            Member::Changing { to, .. }
                            | Member::Entrance { to, .. }
                            | Member::Floating { to, .. } => to,
                        };
                        let got = dest(&merged, w)
                            .unwrap_or_else(|| panic!("{tag}: {w:?} unnamed after merge"));
                        // The one P4 exception: a member sent off the viewport rides its moving container (`rides_out`).
                        let rode_out = pan.is_none()
                            && rini_geometry::is_off_screen(DISPLAY, want)
                            && matches!(before.member(w), Some(Member::Rigid { key, .. })
                                if before.groups.iter().any(|g| g.key == key && !g.is_still()));
                        if rode_out {
                            assert_eq!(
                                key_of(&merged, w),
                                key_of(&before, w),
                                "{tag}: rides its container"
                            );
                            continue;
                        }
                        assert!(
                            (got.origin.x - want.origin.x).abs() <= GROUP_TOLERANCE + 0.01
                                && (got.origin.y - want.origin.y).abs() <= GROUP_TOLERANCE + 0.01,
                            "{tag}: {w:?} ends at {got:?}, pass wants {want:?}"
                        );
                        if let Some((_, _, to_key)) =
                            delta.reparented.iter().find(|(x, _, _)| *x == w)
                        {
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
                        assert_eq!(
                            delta.retargeted_groups.is_empty(),
                            is_zero(d) || before.groups.iter().all(|g| g.members.is_empty()),
                            "{tag}"
                        );
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
                let presented = if rng.coin() {
                    at_model(&current)
                } else {
                    midway(&current)
                };
                // The same destinations, expressed from the presented frames.
                let mut reqs: Vec<(WindowId, CGRect, CGRect, bool)> = Vec::new();
                for &(w, _, _, floating) in &requests {
                    let (from, to) = match current.member(w).unwrap() {
                        Member::Rigid { key, rel } => (
                            overlay_of(rel, presented[&key]),
                            overlay_of(rel, current.position_of(key)),
                        ),
                        Member::Changing { from, to } | Member::Entrance { from, to } => (from, to),
                        Member::Floating { from, to } => (
                            overlay_of(from, presented[&GroupKey::Floating]),
                            overlay_of(to, current.position_of(GroupKey::Floating)),
                        ),
                    };
                    reqs.push((w, from, to, floating));
                }
                let same = reflow_plan(&reqs, DISPLAY);
                let (merged, delta) =
                    merge_plans(&current, &same, None, &presented.clone().into(), None, DISPLAY);
                assert!(delta.is_empty(), "seed 151 run {run}: {delta:?}");
                assert_eq!(merged, current, "seed 151 run {run}");
            }
        }
    }

    /// Task 1 of `.kiro/specs/rigid-strip-groups`: a pass as rigid pieces. See "Layout changes" and
    /// "Strip movements" in `src/animation/docs/animation-smoothness.md`.
    mod rigid_groups {
        use super::preservation::{DISPLAY, Gen, RUNS, stacked};
        use super::*;
        use crate::animation::platform::engine::plan::*;
        use crate::animation::platform::window_snapshot::is_a_resize;

        fn wid(idx: u32) -> WindowId {
            WindowId {
                pid: 7,
                idx: std::num::NonZeroU32::new(idx).unwrap(),
            }
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
        fn two_columns_shifting_by_the_same_vector_are_one_group() {
            let a = column(0.0);
            let b = column(1.0);
            let plan = reflow_plan(
                &[
                    (wid(1), a, shifted(a, -859.0, 0.0), false),
                    (wid(2), b, shifted(b, -859.0, 0.0), false),
                ],
                DISPLAY,
            );
            let groups = moving(&plan);
            assert_eq!(groups.len(), 1);
            assert_eq!(groups[0].travel, CGPoint::new(-859.0, 0.0));
            assert_eq!(members(groups[0]), vec![wid(1), wid(2)]);
            assert_eq!(
                groups[0].members[0].rel, a,
                "a container installs at (0,0), so rel is from"
            );
            assert!(plan.groups[0].members.is_empty(), "nothing stands still");
            assert!(
                plan.changing.is_empty() && plan.floating.is_empty() && plan.entrances.is_empty()
            );
        }

        #[test]
        fn a_column_two_points_off_shares_the_group_and_three_points_off_opens_one() {
            let a = column(0.0);
            let b = column(1.0);
            let same = reflow_plan(
                &[
                    (wid(1), a, shifted(a, 300.0, 0.0), false),
                    (wid(2), b, shifted(b, 302.0, 0.0), false),
                ],
                DISPLAY,
            );
            assert_eq!(moving(&same).len(), 1, "2pt apart: one group");
            assert_eq!(members(moving(&same)[0]), vec![wid(1), wid(2)]);

            let split = reflow_plan(
                &[
                    (wid(1), a, shifted(a, 300.0, 0.0), false),
                    (wid(2), b, shifted(b, 303.0, 0.0), false),
                ],
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
            assert_eq!(
                plan.member(wid(1)),
                Some(Member::Changing { from: a, to: grown })
            );
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
            assert_eq!(
                members(moving(&plan)[0]),
                vec![wid(1)],
                "the same vector does not pull it in"
            );
            assert_eq!(
                plan.floating,
                vec![
                    (wid(2), settings, shifted(settings, -100.0, 0.0)),
                    (wid(3), settings, settings)
                ]
            );
            assert!(plan.group_of(wid(2)).is_none() && plan.group_of(wid(3)).is_none());
            assert_eq!(
                plan.floating_travel,
                CGPoint::new(0.0, 0.0),
                "a layout pass never moves the container"
            );
        }

        #[test]
        fn the_still_window_is_in_the_still_group() {
            let a = column(0.0);
            let b = column(1.0);
            let plan = reflow_plan(
                &[
                    (wid(1), a, a, false),
                    (wid(2), b, shifted(b, 40.0, 0.0), false),
                ],
                DISPLAY,
            );
            assert_eq!(plan.groups[0].key, GroupKey::STILL);
            assert_eq!(plan.groups[0].travel, CGPoint::new(0.0, 0.0));
            assert_eq!(members(&plan.groups[0]), vec![wid(1)]);
            assert_eq!(
                plan.member(wid(1)),
                Some(Member::Rigid { key: GroupKey::STILL, rel: a })
            );
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
            let park = rect(
                DISPLAY.size.width - 1.0,
                DISPLAY.size.height - 1.0,
                a.size.width,
                a.size.height,
            );
            let travel = neighbour_travel(travel_subject(a, park, DISPLAY), &[], DISPLAY);
            assert_eq!(travel, None);
            let a_end = resolve_end(a, park, DISPLAY, travel);
            assert_eq!(a_end, rini_geometry::park_entry_frame(park, a, DISPLAY));
            let still = column(0.0);

            let plan = reflow_plan(
                &[(wid(1), a, a_end, false), (wid(2), still, still, false)],
                DISPLAY,
            );
            let groups = moving(&plan);
            assert_eq!(groups.len(), 1);
            assert_eq!(members(groups[0]), vec![wid(1)]);
            assert_eq!(groups[0].travel, CGPoint::new(a_end.origin.x - a.origin.x, 0.0));
            assert_eq!(members(&plan.groups[0]), vec![wid(2)]);
        }

        fn strip_window(idx: u32, frame: CGRect, pinned: bool, floating: bool) -> SurfaceWindow {
            SurfaceWindow {
                window: wid(idx),
                server_id: WindowServerId::new(idx),
                frame,
                pinned,
                floating,
                from: None,
            }
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
                assert_eq!(
                    overlay_of(member.rel, groups[0].travel),
                    to,
                    "rel plus travel is the destination"
                );
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
            let plan =
                plan::surface_plan(&windows, CGPoint::new(-574.0, 0.0), CGPoint::new(0.0, 0.0));
            assert_eq!(plan.floating, vec![(wid(2), settings, settings)]);
            assert_eq!(plan.floating_travel, CGPoint::new(0.0, 0.0));
            assert_eq!(
                plan.member(wid(2)),
                Some(Member::Floating { from: settings, to: settings })
            );
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
            assert_eq!(
                plan.floating,
                vec![(wid(2), settings, settings)],
                "the tile itself stands in its container"
            );
            let (_, to) = surface_travel(settings, from_offset, to_offset, false);
            assert_eq!(overlay_of(settings, plan.floating_travel), to);
        }

        #[test]
        fn group_relative_and_overlay_of_are_inverses() {
            let f = rect(867.0, 32.0, 859.0, 1081.0);
            let p = CGPoint::new(-574.0, 12.0);
            assert_eq!(overlay_of(group_relative(f, p), p), f);
            assert_eq!(group_relative(f, p), rect(1441.0, 20.0, 859.0, 1081.0));
            assert_eq!(
                group_relative(f, CGPoint::new(0.0, 0.0)),
                f,
                "at install rel is from"
            );
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
            let plan = reflow_plan(
                &[
                    (wid(1), a, shifted(a, -100.0, 0.0), false),
                    (wid(2), a, a, false),
                ],
                DISPLAY,
            );
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
                let v = CGPoint::new(
                    rng.pt(-12.0, 12.0) * 50.0,
                    if rng.coin() {
                        0.0
                    } else {
                        rng.pt(-6.0, 6.0) * 50.0
                    },
                );
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
                            let to = rect(
                                from.origin.x,
                                from.origin.y,
                                from.size.width + rng.pt(-300.0, 300.0),
                                from.size.height,
                            );
                            let to = if is_a_resize(from.size, to.size) {
                                to
                            } else {
                                rect(
                                    to.origin.x,
                                    to.origin.y,
                                    from.size.width + 200.0,
                                    to.size.height,
                                )
                            };
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
                let display = if run % 2 == 0 {
                    DISPLAY
                } else {
                    rect(1728.0, -300.0, 3008.0, 1692.0)
                };
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
                    assert!(
                        i == 0 || !group.members.is_empty(),
                        "{tag}: an empty moving group"
                    );
                }

                for &(window, start, end, floating) in &requests {
                    let from = to_overlay_space(start, display);
                    let to = to_overlay_space(end, display);
                    let v = CGPoint::new(to.origin.x - from.origin.x, to.origin.y - from.origin.y);
                    match plan.member(window).unwrap_or_else(|| panic!("{tag}: {window:?} unnamed"))
                    {
                        Member::Rigid { key, rel } => {
                            assert!(!floating, "{tag}: a floating window grouped");
                            assert!(!is_a_resize(from.size, to.size), "{tag}: a resize grouped");
                            assert_eq!(rel, from, "{tag}");
                            let group = plan.group_of(window).unwrap();
                            assert_eq!(group.key, key, "{tag}");
                            assert!(
                                same_vector(group.travel, v),
                                "{tag}: {v:?} in group at {:?}",
                                group.travel
                            );
                            if !is_moving(from, to) {
                                assert_eq!(
                                    key,
                                    GroupKey::STILL,
                                    "{tag}: a still window off the still group"
                                );
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
                                    (va.x - vb.x).abs() > GROUP_TOLERANCE
                                        || (va.y - vb.y).abs() > GROUP_TOLERANCE,
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
                &[
                    (wid(1), left, left, false),
                    (wid(2), neighbour, shifted(neighbour, 859.0, 0.0), false),
                ],
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
            let grown = rect(
                middle.origin.x,
                middle.origin.y,
                middle.size.width + dw,
                middle.size.height,
            );
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
            assert!(
                plan.member(closed).is_none(),
                "nothing is drawn for the closed window"
            );
            assert_eq!(moving(&plan).len(), 1);
            assert_eq!(members(moving(&plan)[0]), vec![wid(3), wid(4)]);
            assert!(!worth_flying(false, false));
            assert!(worth_flying(true, false));
            assert!(worth_flying(false, true));
        }

        #[test]
        fn a_floating_only_pass_has_no_groups_and_one_floating_member() {
            let settings = rect(500.0, 300.0, 700.0, 500.0);
            let plan = reflow_plan(
                &[(wid(1), settings, shifted(settings, 40.0, 20.0), true)],
                DISPLAY,
            );
            assert!(moving(&plan).is_empty());
            assert!(plan.groups[0].members.is_empty());
            assert_eq!(
                plan.floating,
                vec![(wid(1), settings, shifted(settings, 40.0, 20.0))]
            );
            let flight = FlightPlan::from(plan);
            let targets = crate::animation::platform::overlay::animation_targets(&flight);
            assert_eq!(targets.len(), 1, "the floating tile flies on its own");
        }

        #[test]
        fn a_grow_still_enters_awaiting() {
            use crate::animation::platform::window_snapshot::{outgrows, test_snapshot};
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
                refresh: FocusRefresh::default(),
                harvested: HashSet::new(),
                focus: None,
                plan: FlightPlan::empty(),
                nudge: None,
                _clock: None,
            };
            let frames = running.extend_hold(
                &[(wid(1), grown.size)],
                false,
                running.duration,
                Instant::now(),
            );
            assert_eq!(running.awaiting, vec![(wid(1), grown.size)]);
            assert!(running.hold_deadline.is_some());
            assert_eq!(
                frames,
                Some(vec![(wid(1), grown)]),
                "the held frames go out under the overlay"
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
            let final_frames = vec![(wid(1), grown), (wid(2), slot)];
            let (holding, chase_set, now) = frame_zero_work(
                &[(wid(1), grown.size)],
                &[(wid(2), slot.size)],
                &final_frames,
                &[(wid(2), slot)],
            );
            assert!(holding);
            assert_eq!(
                now, final_frames,
                "holding: every frame goes out under the overlay"
            );
            assert_eq!(chase_set, vec![(wid(1), grown.size), (wid(2), slot.size)]);

            let (holding, chase_set, now) =
                frame_zero_work(&[], &[(wid(2), slot.size)], &final_frames, &[(wid(2), slot)]);
            assert!(!holding);
            assert_eq!(
                now,
                vec![(wid(2), slot)],
                "not holding: the newcomer's slot alone"
            );
            assert_eq!(chase_set, vec![(wid(2), slot.size)]);

            let (holding, chase_set, now) = frame_zero_work(&[], &[], &final_frames, &[]);
            assert!(!holding && chase_set.is_empty() && now.is_empty());
        }

        #[test]
        fn a_spawn_entrance_flies_without_a_hold_and_is_a_reveal_in_waiting() {
            use crate::animation::platform::window_snapshot::test_snapshot;
            let slot = rect(867.0, 32.0, 859.0, 1081.0);
            let spawn = rect(300.0, 200.0, 400.0, 300.0);
            let EntranceDecision::Travel { from, to } =
                entrance_plan(Some(spawn), slot, true, true)
            else {
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
                companion: None,
                focused: true,
            };
            let awaiting: Vec<(WindowId, CGSize)> = Vec::new();
            let (holding, chase_set, now) = frame_zero_work(
                &awaiting,
                &[(wid(9), slot.size)],
                &[(wid(9), slot)],
                &[(wid(9), slot)],
            );
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
                hold_deadline: holding
                    .then(|| Instant::now() + reveal_hold_limit(Duration::from_millis(350))),
                refresh: FocusRefresh::default(),
                harvested: HashSet::new(),
                focus: Some(wid(9)),
                plan: FlightPlan::empty(),
                nudge: None,
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
            assert_eq!(
                running.tile_state(wid(9), &test_snapshot(slot.size)),
                TileState::Reveal { fits: true }
            );
            // The plan calls it an entrance; the tile is a loose resize from spawn to slot.
            let mut plan = ReflowPlan::empty();
            plan.entrances.push((wid(9), from, to));
            assert_eq!(plan.member(wid(9)), Some(Member::Entrance { from, to }));
            let targets =
                crate::animation::platform::overlay::animation_targets(&FlightPlan::from(plan));
            assert_eq!(targets.len(), 1);
        }

        #[test]
        fn a_spawn_entrances_early_chase_picture_is_taken_without_a_release() {
            use crate::animation::platform::window_snapshot::test_snapshot;
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
                refresh: FocusRefresh::default(),
                harvested: HashSet::new(),
                focus: None,
                plan: FlightPlan::empty(),
                nudge: None,
                _clock: None,
            };
            running.tiles[0].snapshot = test_snapshot(spawn.size);
            assert_eq!(
                running.claim(wid(9), &test_snapshot(spawn.size)),
                None,
                "still spawn-sized"
            );
            assert_eq!(
                running.claim(wid(9), &test_snapshot(slot.size)),
                Some(Claimed::Refreshed)
            );
            assert!(running.tiles[0].snapshot.fits(slot.size));
            assert_eq!(
                running.claim(wid(77), &test_snapshot(slot.size)),
                None,
                "no such tile"
            );
        }

        /// Property (seed 157, 200 runs).
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

        /// The 50/50 pair with Settings over them (`motion/z_group.rs`).
        #[test]
        fn band_plan_puts_the_floating_container_behind_the_strip_unless_it_holds_focus() {
            use crate::animation::domain::motion::z_group::GROUP_STRIDE;
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
            companion.companion = Some(wid(90));
            tiles.push(companion);
            restack(&mut tiles, Some(wid(90)));
            let plan = FlightPlan::from(plan_from_tiles(&tiles));
            let still = plan.groups[0].key;
            let moving = plan.groups[1].key;

            let banding = band_plan(&plan, &tiles, Some(wid(90)));
            assert!(!banding.focus_off_strip);
            assert!(banding.lifted.is_empty(), "nothing is lifted over the strip");
            assert_eq!(
                banding.group_order,
                vec![still, moving],
                "the focused group first"
            );
            assert_eq!(
                banding.within[&wid(90)],
                0,
                "the focused window leads its container"
            );
            assert_eq!(banding.within[&wid(89)], 3, "one past its server order");
            assert_eq!(banding.within[&wid(5830)], 2);
            let anchor = tiles.iter().find(|t| t.window == wid(90)).unwrap().depth;
            assert_eq!(
                banding.within[&wid(900)],
                anchor % GROUP_STRIDE,
                "a companion takes its window's depth"
            );

            restack(&mut tiles, Some(wid(5830)));
            let banding = band_plan(&plan, &tiles, Some(wid(5830)));
            assert!(banding.focus_off_strip);
            assert_eq!(banding.lifted, vec![wid(5830)]);
            assert_eq!(banding.within[&wid(5830)], 0);
            assert_eq!(
                banding.group_order,
                vec![still, moving],
                "no strip group holds focus: shallowest first"
            );
        }

        /// The reported flight: switching to 1Password with zoom behind the strip. Only 1Password's
        /// application is lifted; zoom stays in the floating container behind the strip.
        #[test]
        fn band_plan_lifts_only_the_application_gaining_focus() {
            let app = |pid: i32, idx: u32| WindowId {
                pid,
                idx: std::num::NonZeroU32::new(idx).unwrap(),
            };
            let (column, onepassword, zoom, controls) =
                (app(10, 1), app(20, 2), app(30, 3), app(30, 4));
            let slot = rect(4.0, 32.0, 1720.0, 1081.0);
            let (card, meeting, bar) = (
                rect(414.0, 35.0, 900.0, 1079.0),
                rect(224.0, 95.0, 1280.0, 960.0),
                rect(600.0, 900.0, 400.0, 60.0),
            );
            let mut tiles = vec![
                stacked(column, slot, slot, Some(0), false),
                stacked(onepassword, entrance_from(card), card, Some(5), true),
                stacked(zoom, meeting, meeting, Some(3), true),
                stacked(controls, bar, bar, Some(4), true),
            ];
            restack(&mut tiles, Some(onepassword));
            let plan = FlightPlan::from(plan_from_tiles(&tiles));
            let banding = band_plan(&plan, &tiles, Some(onepassword));
            assert_eq!(banding.lifted, vec![onepassword], "zoom is not lifted with it");

            // Focusing either zoom window lifts both of them: they are one application.
            restack(&mut tiles, Some(controls));
            let banding = band_plan(&plan, &tiles, Some(controls));
            let mut lifted = banding.lifted.clone();
            lifted.sort();
            assert_eq!(lifted, vec![zoom, controls]);
        }

        /// Property P3 (seed 163, 200 runs): `container_z - within` is `-depth` exactly, the lifted
        /// floating windows are in front of every strip tile, and the rest are behind every one.
        /// Pids are drawn from three applications so the application rule is exercised.
        #[test]
        fn container_bands_plus_within_depths_reproduce_every_depth() {
            use crate::animation::domain::motion::z_group::{Band, container_z, stack};
            let mut rng = Gen(163);
            for run in 0..RUNS {
                let count = 1 + rng.below(8) as usize;
                let mut tiles: Vec<OverlayTile> = (0..count)
                    .map(|i| {
                        let f = rng.on_screen();
                        let order = if rng.below(6) == 0 {
                            None
                        } else {
                            Some(rng.below(40) as usize)
                        };
                        let window = WindowId {
                            pid: 1 + rng.below(3) as i32,
                            idx: std::num::NonZeroU32::new(i as u32 + 1).unwrap(),
                        };
                        stacked(window, f, f, order, rng.coin())
                    })
                    .collect();
                let focus = if rng.coin() {
                    Some(tiles[rng.below(count as u64) as usize].window)
                } else {
                    None
                };
                restack(&mut tiles, focus);
                let plan = FlightPlan::from(plan_from_tiles(&tiles));
                let banding = band_plan(&plan, &tiles, focus);
                let tag = format!("seed 163 run {run}");
                let placements = stack(&stacked_windows(&tiles), focus);
                let mut strip_total: Vec<f64> = Vec::new();
                let mut lifted_total: Vec<f64> = Vec::new();
                let mut behind_total: Vec<f64> = Vec::new();
                for (tile, placement) in tiles.iter().zip(&placements) {
                    let within = banding.within[&tile.window];
                    let total =
                        container_z(placement.band, banding.focus_off_strip) - within as f64;
                    assert_eq!(total, -(tile.depth as f64), "{tag}: {:?}", tile.window);
                    assert_eq!(
                        banding.lifted.contains(&tile.window),
                        placement.band == Band::Lifted,
                        "{tag}: {:?}",
                        tile.window
                    );
                    match placement.band {
                        Band::Strip => strip_total.push(total),
                        Band::Lifted => lifted_total.push(total),
                        Band::Behind => behind_total.push(total),
                    }
                }
                for s in &strip_total {
                    for f in &lifted_total {
                        assert!(f > s, "{tag}: lifted {f} behind strip {s}");
                    }
                    for f in &behind_total {
                        assert!(f < s, "{tag}: floating {f} in front of strip {s}");
                    }
                }
                if let Some(focus) = focus {
                    let focus_floating = tiles.iter().any(|t| t.window == focus && t.floating);
                    for tile in tiles.iter().filter(|t| t.floating && t.window.pid == focus.pid) {
                        assert_eq!(
                            banding.lifted.contains(&tile.window),
                            focus_floating,
                            "{tag}: the focused application comes forward as a set"
                        );
                    }
                }
                // Every occupied strip container is ordered once; the floating one never is.
                let mut order = banding.group_order.clone();
                order.sort_by_key(|k| format!("{k:?}"));
                order.dedup();
                assert_eq!(order.len(), banding.group_order.len(), "{tag}");
                assert!(!banding.group_order.contains(&GroupKey::Floating), "{tag}");
                for g in plan.groups.iter().filter(|g| !g.members.is_empty()) {
                    assert!(
                        banding.group_order.contains(&g.key),
                        "{tag}: {:?} unordered",
                        g.key
                    );
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
                        stacked(
                            window,
                            to_overlay_space(start, DISPLAY),
                            to_overlay_space(end, DISPLAY),
                            None,
                            floating,
                        )
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
            use crate::animation::platform::overlay::{AnimationTarget, animation_targets};
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
                            assert_eq!(
                                from,
                                CGPoint::new(0.0, 0.0),
                                "{tag}: a fresh flight installs at the origin"
                            );
                            assert_eq!(to, travel, "{tag}");
                            assert!(
                                travel.x != 0.0 || travel.y != 0.0,
                                "{tag}: a zero-travel container"
                            );
                        }
                        AnimationTarget::Tile { window, .. } => {
                            assert!(!tiles.contains(&window), "{tag}: {window:?} named twice");
                            tiles.push(window);
                        }
                    }
                }
                for group in &plan.groups {
                    for member in &group.members {
                        assert!(
                            !tiles.contains(&member.window),
                            "{tag}: rigid {:?} as a tile",
                            member.window
                        );
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
                    .chain(
                        plan.floating.iter().filter(|(_, f, t)| !f.same_as(*t)).map(|(w, _, _)| *w),
                    )
                    .collect();
                expected.sort();
                tiles.sort();
                assert_eq!(tiles, expected, "{tag}: tile targets");
            }
        }
    }

    /// A window moved along the strip: the two columns changing places cross the surface as pieces
    /// of their own, the moved one in front, and a burst of moves merges as one movement. See "The
    /// move flight" in `src/animation/docs/animation-smoothness.md`.
    mod strip_move {
        use super::preservation::{DISPLAY, stacked};
        use super::*;
        use crate::animation::domain::motion::glide::Leg;
        use crate::animation::domain::motion::strip_move::swap_starts;
        use crate::animation::platform::engine::plan::*;
        use crate::animation::platform::window_snapshot::test_snapshot;
        use rini_ipc::protocol::Direction;

        /// A full-width column and the gap after it.
        const WIDTH: f64 = 1720.0;
        const STEP: f64 = 1723.0;

        fn wid(idx: u32) -> WindowId {
            WindowId {
                pid: 7,
                idx: std::num::NonZeroU32::new(idx).unwrap(),
            }
        }

        fn at(x: f64, width: f64) -> CGRect {
            rect(x, 32.0, width, 1081.0)
        }

        /// The surface of one move: every window at its new frame, the swapped pair from their old
        /// slots, as `start_strip_pan` builds it.
        fn surface(
            frames: &[(WindowId, CGRect)],
            moved: WindowId,
            direction: Direction,
        ) -> Vec<SurfaceWindow> {
            let starts = swap_starts(frames, moved, direction);
            frames
                .iter()
                .map(|&(window, frame)| SurfaceWindow {
                    window,
                    server_id: WindowServerId::new(window.idx.get()),
                    frame,
                    pinned: false,
                    floating: false,
                    from: starts.iter().find(|(w, _)| *w == window).map(|(_, start)| *start),
                })
                .collect()
        }

        fn dest(plan: &FlightPlan, window: WindowId) -> Option<CGRect> {
            match plan.member(window)? {
                Member::Rigid { key, rel } => Some(overlay_of(rel, plan.position_of(key))),
                _ => None,
            }
        }

        fn key_of(plan: &FlightPlan, window: WindowId) -> Option<GroupKey> {
            match plan.member(window)? {
                Member::Rigid { key, .. } => Some(key),
                _ => None,
            }
        }

        /// Half-width columns, W at the right edge moving right past B, the strip scrolling one column
        /// with it. B is two windows stacked. W stands still on screen, B crosses it, the rest pans.
        #[test]
        fn the_moved_column_and_the_one_it_passed_are_pieces_of_their_own() {
            let top = rect(4.0, 32.0, 860.0, 539.0);
            let bottom = rect(4.0, 574.0, 860.0, 539.0);
            let frames = [
                (wid(1), at(4.0 - 864.0, 860.0)),
                (wid(2), top),
                (wid(3), bottom),
                (wid(4), at(868.0, 860.0)),
                (wid(5), at(1732.0, 860.0)),
            ];
            let windows = surface(&frames, wid(4), Direction::Right);
            let (from_offset, to_offset) = (CGPoint::new(-864.0, 0.0), CGPoint::new(0.0, 0.0));
            let plan = surface_plan(&windows, from_offset, to_offset);
            let group = |window| plan.group_of(window).expect("rigid").clone();

            assert_eq!(group(wid(4)).key, GroupKey::STILL, "the camera followed it");
            assert_eq!(group(wid(4)).members.len(), 1);
            let passed = group(wid(2));
            assert_eq!(passed.key, group(wid(3)).key, "a stacked column is one piece");
            assert_eq!(passed.travel, CGPoint::new(-1728.0, 0.0));
            let rest = group(wid(1));
            assert_eq!(rest.key, group(wid(5)).key, "the rest of the strip is one piece");
            assert_eq!(rest.travel, pan_travel(from_offset, to_offset));
            assert_eq!(plan.groups.iter().filter(|g| !g.members.is_empty()).count(), 3);
            for window in &windows {
                let (from, to) = window.travel(from_offset, to_offset);
                let member = group(window.window);
                let rel = member.members.iter().find(|m| m.window == window.window).unwrap().rel;
                assert_eq!(rel, from, "installs where it was on screen");
                assert_eq!(overlay_of(rel, member.travel), to, "lands at its new frame");
            }
        }

        /// The moved window is focused and its container is drawn first, so the column it passes
        /// slides beneath it even where the server has that column in front.
        #[test]
        fn the_moved_window_is_drawn_over_the_column_it_passes() {
            let frames = [
                (wid(1), at(4.0 - STEP, WIDTH)),
                (wid(2), at(4.0, WIDTH)),
                (wid(3), at(4.0 + STEP, WIDTH)),
            ];
            let windows = surface(&frames, wid(2), Direction::Right);
            let (from_offset, to_offset) = (CGPoint::new(-STEP, 0.0), CGPoint::new(0.0, 0.0));
            let plan = FlightPlan::from(surface_plan(&windows, from_offset, to_offset));
            let mut tiles: Vec<OverlayTile> = windows
                .iter()
                .enumerate()
                .map(|(order, w)| {
                    let (from, to) = w.travel(from_offset, to_offset);
                    stacked(w.window, from, to, Some(2 - order), false)
                })
                .collect();
            restack(&mut tiles, Some(wid(2)));
            let banding = band_plan(&plan, &tiles, Some(wid(2)));
            let moved = key_of(&plan, wid(2)).unwrap();
            let passed = key_of(&plan, wid(1)).unwrap();
            assert_ne!(moved, passed);
            let order = |key| banding.group_order.iter().position(|k| *k == key).unwrap();
            assert_eq!(order(moved), 0, "the moved window leads");
            assert!(order(moved) < order(passed));
        }

        #[test]
        fn a_nudge_rides_only_a_container_holding_the_moved_window_alone() {
            let frames = [(wid(1), at(4.0 - STEP, WIDTH)), (wid(2), at(4.0, WIDTH))];
            let windows = surface(&frames, wid(2), Direction::Right);
            let mut plan = surface_plan(&windows, CGPoint::new(-STEP, 0.0), CGPoint::new(0.0, 0.0));
            let key = plan.group_of(wid(2)).unwrap().key;
            let mut companion = stacked(wid(20), at(4.0, WIDTH), at(4.0, WIDTH), None, false);
            companion.companion = Some(wid(2));
            plan.adopt(&companion);
            let plan = FlightPlan::from(plan);
            assert_eq!(plan.nudge_carrier(wid(2)), Some(key), "its border rides with it");

            let pan = [
                SurfaceWindow { from: None, ..windows[0] },
                SurfaceWindow { from: None, ..windows[1] },
            ];
            let panned = FlightPlan::from(surface_plan(
                &pan,
                CGPoint::new(-STEP, 0.0),
                CGPoint::new(0.0, 0.0),
            ));
            assert_eq!(
                panned.nudge_carrier(wid(2)),
                None,
                "sharing the strip's container, the whole strip would be nudged"
            );
            assert_eq!(panned.nudge_carrier(wid(9)), None);
        }

        /// A border four points proud of `frame` all round.
        fn hug(frame: CGRect) -> CGRect {
            rect(
                frame.origin.x - 4.0,
                frame.origin.y - 4.0,
                frame.size.width + 8.0,
                frame.size.height + 8.0,
            )
        }

        fn floating_at(window: WindowId, frame: CGRect, pinned: bool) -> SurfaceWindow {
            SurfaceWindow {
                window,
                server_id: WindowServerId::new(window.idx.get()),
                frame,
                pinned,
                floating: true,
                from: None,
            }
        }

        /// A move the camera follows leaves W in the still piece, and a pinned floating window's
        /// border has no travel either. It stands with its window in the floating container and is
        /// drawn in its band, so the nudge carries W and W's own border only; another window's
        /// border in W's piece keeps the nudge off.
        #[test]
        fn a_floating_windows_border_stays_with_it_and_off_the_nudge() {
            let (w, f) = (wid(2), wid(7));
            let settings = rect(500.0, 300.0, 700.0, 500.0);
            let mut windows = surface(
                &[(wid(1), at(4.0 - STEP, WIDTH)), (w, at(4.0, WIDTH))],
                w,
                Direction::Right,
            );
            windows.push(floating_at(f, settings, true));
            let mut plan = surface_plan(&windows, OFFSET, REST);
            let w_key = plan.group_of(w).unwrap().key;
            assert_eq!(w_key, GroupKey::STILL, "the camera followed it");
            let mut tiles: Vec<OverlayTile> = windows
                .iter()
                .enumerate()
                .map(|(order, s)| {
                    let (from, to) = s.travel(OFFSET, REST);
                    stacked(s.window, from, to, Some(order), s.floating)
                })
                .collect();
            restack(&mut tiles, Some(f));
            let tile = |window| tiles.iter().find(|t| t.window == window).unwrap().clone();
            let border = |anchor: WindowId, real: CGRect, window| {
                border_tile(
                    &tile(anchor),
                    real,
                    (window, hug(real)),
                    test_snapshot(hug(real).size),
                )
            };
            let f_border = border(f, settings, wid(70));
            let w_border = border(w, at(4.0, WIDTH), wid(20));
            assert!(f_border.floating, "a floating window's border is floating");
            assert_eq!(f_border.companion, Some(f));
            assert_eq!(f_border.depth, tile(f).depth);
            assert!(!w_border.floating);
            plan.adopt(&f_border);
            plan.adopt(&w_border);
            let flight = FlightPlan::from(plan.clone());
            assert_eq!(
                flight.member(wid(70)),
                Some(Member::Floating {
                    from: hug(settings),
                    to: hug(settings)
                }),
                "it stands with F"
            );
            assert_eq!(key_of(&flight, wid(20)), Some(w_key), "W's border rides with W");
            assert_eq!(flight.nudge_carrier(w), Some(w_key));
            tiles.extend([f_border, w_border]);
            let banding = band_plan(&flight, &tiles, Some(f));
            assert!(banding.lifted.contains(&f));
            assert!(banding.lifted.contains(&wid(70)), "drawn in front with F");

            let mut other = stacked(wid(21), at(4.0, WIDTH), at(4.0, WIDTH), None, false);
            other.companion = Some(wid(1));
            plan.adopt(&other);
            assert_eq!(
                FlightPlan::from(plan).nudge_carrier(w),
                None,
                "another window's border would be nudged with W"
            );
        }

        /// On a switch the floating container carries its windows. A floating window's border rides
        /// it the same way, not a second time on a movement of its own.
        #[test]
        fn a_floating_windows_border_rides_the_floating_container_on_a_switch() {
            let f = wid(7);
            let settings = rect(500.0, 300.0, 700.0, 500.0);
            let windows = [floating_at(f, settings, false)];
            let from_offset = CGPoint::new(0.0, -1117.0);
            let mut plan = surface_plan(&windows, from_offset, REST);
            assert!(plan.floating_travel.y != 0.0, "the container travels");
            let (from, to) = windows[0].travel(from_offset, REST);
            let border = border_tile(
                &stacked(f, from, to, Some(0), true),
                settings,
                (wid(70), hug(settings)),
                test_snapshot(hug(settings).size),
            );
            plan.adopt(&border);
            let flight = FlightPlan::from(plan);
            let still = |window| match flight.member(window) {
                Some(Member::Floating { from, to }) => from.same_as(to),
                other => panic!("{other:?}"),
            };
            assert!(still(f));
            assert!(still(wid(70)), "it rides the container and nothing else");
        }

        /// A strip movement starts at once and never holds, so a stand-in's real picture is chased
        /// instead: for the column a move at the edge brings in from its park, not for one parked
        /// at both ends, and never for a window drawn from its own picture.
        #[test]
        fn a_stand_in_coming_on_screen_is_chased_and_one_staying_parked_is_not() {
            use crate::animation::platform::window_snapshot::{placeholder, test_bitmap};
            let slot = rect(868.0, 32.0, 860.0, 1081.0);
            let park = rect(1732.0, 32.0, 860.0, 1081.0);
            let stand_in = |window, from: CGRect, to: CGRect| {
                let mut tile = stacked(window, from, to, Some(1), false);
                tile.snapshot = placeholder(to.size, test_bitmap());
                tile
            };
            let tiles = [
                stand_in(wid(1), park, slot),
                stand_in(wid(2), at(4.0 - 3.0 * STEP, WIDTH), at(4.0 - 2.0 * STEP, WIDTH)),
                stacked(wid(3), park, slot, Some(2), false),
            ];
            assert_eq!(stand_in_chase(&tiles, DISPLAY), vec![(wid(1), slot.size)]);
        }

        /// The viewport's travel of one full-width move the camera follows.
        const OFFSET: CGPoint = CGPoint { x: -STEP, y: 0.0 };
        const REST: CGPoint = CGPoint { x: 0.0, y: 0.0 };

        /// Full-width columns 0 A W B C (`wid(1)` to `wid(5)`), W moved right twice in a row: the
        /// flight of the first press, and the frames and plan of the second.
        fn two_presses() -> (FlightPlan, [(WindowId, CGRect); 5], ReflowPlan) {
            let (zero, a, w, b, c) = (wid(1), wid(2), wid(3), wid(4), wid(5));
            let first = [
                (zero, at(4.0 - 2.0 * STEP, WIDTH)),
                (a, at(4.0 - STEP, WIDTH)),
                (w, at(4.0, WIDTH)),
                (b, at(4.0 + STEP, WIDTH)),
                (c, at(4.0 + 2.0 * STEP, WIDTH)),
            ];
            let current =
                FlightPlan::from(surface_plan(&surface(&first, w, Direction::Right), OFFSET, REST));
            let second = [
                (zero, at(4.0 - 3.0 * STEP, WIDTH)),
                (a, at(4.0 - 2.0 * STEP, WIDTH)),
                (b, at(4.0 - STEP, WIDTH)),
                (w, at(4.0, WIDTH)),
                (c, at(4.0 + STEP, WIDTH)),
            ];
            let incoming = surface_plan(&surface(&second, w, Direction::Right), OFFSET, REST);
            (current, second, incoming)
        }

        /// Every container of `plan` half-way along its travel.
        fn halfway(plan: &FlightPlan) -> HashMap<GroupKey, CGPoint> {
            plan.groups
                .iter()
                .map(|g| (g.key, CGPoint::new(g.travel.x / 2.0, g.travel.y / 2.0)))
                .collect()
        }

        /// The frame `window` is drawn at, its container at `presented`.
        fn drawn(
            plan: &FlightPlan,
            presented: &HashMap<GroupKey, CGPoint>,
            window: WindowId,
        ) -> CGRect {
            match plan.member(window) {
                Some(Member::Rigid { key, rel }) => overlay_of(rel, presented[&key]),
                other => panic!("{window:?} is {other:?}"),
            }
        }

        /// The second press arrives while the first still flies: W stays where it is drawn and
        /// keeps its container (and its nudge), B is taken out of the strip's piece and crosses
        /// beneath W, A (passed by the first press) and everything else carry on with the pan. W is
        /// still drawn over the column it passes.
        #[test]
        fn a_second_move_mid_flight_swaps_its_pair_and_pans_the_rest() {
            let (zero, a, w, b, c) = (wid(1), wid(2), wid(3), wid(4), wid(5));
            let (current, second, incoming) = two_presses();
            let (merged, delta) = merge_plans(
                &current,
                &incoming,
                Some(pan_travel(OFFSET, REST)),
                &halfway(&current).into(),
                Some(w),
                DISPLAY,
            );

            for (window, frame) in second {
                assert!(
                    dest(&merged, window).unwrap().same_as(frame),
                    "{window:?} lands at {frame:?}, not {:?}",
                    dest(&merged, window)
                );
            }
            let w_key = key_of(&current, w).unwrap();
            assert_eq!(key_of(&merged, w), Some(w_key), "W keeps its container");
            assert!(
                !delta.retargeted_groups.iter().any(|(key, _)| *key == w_key),
                "W is still going where it was: {:?}",
                delta.retargeted_groups
            );
            let strip = key_of(&current, zero).unwrap();
            assert_eq!(key_of(&current, b), Some(strip));
            assert!(delta.reparented.contains(&(b, strip, key_of(&merged, b).unwrap())));
            assert_eq!(
                key_of(&merged, zero),
                Some(strip),
                "the rest keeps the strip's piece"
            );
            assert_eq!(key_of(&merged, c), Some(strip));
            assert_eq!(key_of(&merged, a), key_of(&current, a), "the first pair pans on");

            let mut tiles: Vec<OverlayTile> = second
                .iter()
                .enumerate()
                .map(|(order, &(window, frame))| stacked(window, frame, frame, Some(order), false))
                .collect();
            restack(&mut tiles, Some(w));
            let banding = band_plan(&merged, &tiles, Some(w));
            let order = |window| {
                let key = key_of(&merged, window).unwrap();
                banding.group_order.iter().position(|k| *k == key).unwrap()
            };
            assert_eq!(order(w), 0, "the moved window leads");
            assert!(order(w) < order(b), "B slides beneath it");
        }

        /// The column the second press passes leaves the strip's container for one of its own, from
        /// where it is drawn and exactly as fast as the strip was going. A curve from rest there
        /// launched it at many times the speed it had a frame before.
        #[test]
        fn the_column_a_second_move_passes_keeps_its_speed() {
            let (w, b) = (wid(3), wid(4));
            let (current, _, incoming) = two_presses();
            let seconds = 0.35 * crate::animation::domain::motion::strip_move::NUDGE_STRETCH;
            let legs: HashMap<GroupKey, Leg> = current
                .groups
                .iter()
                .filter(|g| !g.is_still())
                .map(|g| {
                    let leg = Leg::Curve {
                        from: CGPoint::new(0.0, 0.0),
                        to: g.travel,
                        begin: 0.0,
                        seconds,
                    };
                    (g.key, leg)
                })
                .collect();
            let now = 0.12;
            let along: HashMap<GroupKey, CGPoint> = current
                .groups
                .iter()
                .map(|g| {
                    (
                        g.key,
                        legs.get(&g.key).map_or(g.travel, |leg| leg.position_at(now)),
                    )
                })
                .collect();
            let (merged, delta) = merge_plans(
                &current,
                &incoming,
                Some(pan_travel(OFFSET, REST)),
                &along.clone().into(),
                Some(w),
                DISPLAY,
            );
            let strip = key_of(&current, b).unwrap();
            let key = key_of(&merged, b).unwrap();
            let opened =
                delta.new_groups.iter().find(|g| g.key == key).expect("a container of its own");
            assert_eq!(opened.leaving, Some(strip));

            let omega = crate::animation::domain::motion::glide::spring_omega(seconds);
            let to = merged.position_of(key);
            let leg = Leg::leaving(legs.get(&strip), opened.install, to, now, omega);
            let before = drawn(&current, &along, b);
            let after = drawn(&merged, &[(key, leg.position_at(now))].into_iter().collect(), b);
            assert!(after.same_as(before), "no jump: {before:?} then {after:?}");
            let speed = legs[&strip].velocity_at(now).x;
            assert!(speed.abs() > 1000.0, "the strip was moving: {speed}");
            assert!((leg.velocity_at(now).x - speed).abs() < 1e-6, "no kick");
            assert!(
                (leg.position_at(now + leg.seconds()).x - to.x).abs() <= 0.5,
                "and it lands"
            );

            let from_rest = Leg::Curve {
                from: opened.install,
                to,
                begin: now,
                seconds,
            };
            assert!(
                from_rest.velocity_at(now).x / speed > 5.0,
                "what it replaced kicked"
            );
        }

        /// A border whose picture lands between two presses joins the second mid-nudge. It rides
        /// the moved window's container, placed against where that container's leg has it: where it
        /// is drawn carries the nudge, which would have split the border from its window by the
        /// nudge.
        #[test]
        fn a_border_arriving_mid_nudge_rides_with_the_moved_window() {
            let w = wid(3);
            let (mut current, second, mut incoming) = two_presses();
            current.nudging = Some(w);
            let windows = surface(&second, w, Direction::Right);
            let moved = windows.iter().find(|s| s.window == w).unwrap();
            let (from, to) = moved.travel(OFFSET, REST);
            let mut anchor = stacked(w, from, to, Some(0), false);
            anchor.depth = 0;
            let real = at(4.0, WIDTH);
            let frame = rect(0.0, 28.0, WIDTH + 8.0, 1089.0);
            let border = border_tile(&anchor, real, (wid(20), frame), test_snapshot(frame.size));
            incoming.adopt(&border);

            let along = halfway(&current);
            let key = key_of(&current, w).unwrap();
            let mut on_screen = along.clone();
            *on_screen.get_mut(&key).unwrap() = CGPoint::new(576.0, 0.0);
            let presented = Presented {
                drawn: on_screen.clone(),
                along,
            };
            let (merged, delta) = merge_plans(
                &current,
                &incoming,
                Some(pan_travel(OFFSET, REST)),
                &presented,
                Some(w),
                DISPLAY,
            );

            assert_eq!(key_of(&merged, wid(20)), Some(key), "it rides with W");
            assert!(delta.joined_tiles.contains(&(wid(20), key)));
            let (w_at, border_at) = (
                drawn(&merged, &on_screen, w),
                drawn(&merged, &on_screen, wid(20)),
            );
            assert_eq!(
                border_at.origin.x - w_at.origin.x,
                -4.0,
                "where W is drawn, nudge and all"
            );
            assert!(dest(&merged, wid(20)).unwrap().same_as(border.to));
        }

        /// The container a nudge rides takes no one else: a still window joining mid-nudge would be
        /// nudged with it.
        #[test]
        fn a_still_window_joining_mid_nudge_is_not_nudged() {
            let w = wid(3);
            let (mut current, _, _) = two_presses();
            let key = key_of(&current, w).unwrap();
            let stands = rect(900.0, 200.0, 400.0, 300.0);
            let pass = reflow_plan(&[(wid(40), stands, stands, false)], DISPLAY);
            let presented = Presented::from(halfway(&current));

            let (merged, _) = merge_plans(&current, &pass, None, &presented, None, DISPLAY);
            assert_eq!(
                key_of(&merged, wid(40)),
                Some(key),
                "with no nudge it shares the still piece"
            );
            current.nudging = Some(w);
            let (merged, _) = merge_plans(&current, &pass, None, &presented, None, DISPLAY);
            assert_ne!(key_of(&merged, wid(40)), Some(key));
            assert!(dest(&merged, wid(40)).unwrap().same_as(stands));
        }

        /// Where a container is drawn includes the nudge riding it, and a member leaving it mid-nudge
        /// leaves from there, carrying on its container's motion: continuous, and still landing on
        /// its destination.
        #[test]
        fn a_member_leaving_a_nudged_container_leaves_from_where_it_is_drawn() {
            let (a, b) = (at(4.0, 860.0), at(868.0, 860.0));
            let v = CGPoint::new(-300.0, 0.0);
            let current = FlightPlan::from(reflow_plan(
                &[
                    (wid(1), a, rect(a.origin.x + v.x, 32.0, 860.0, 1081.0), false),
                    (wid(2), b, rect(b.origin.x + v.x, 32.0, 860.0, 1081.0), false),
                ],
                DISPLAY,
            ));
            let group = key_of(&current, wid(2)).unwrap();
            let nudge = CGPoint::new(576.0, 0.0);
            let along = CGPoint::new(v.x / 2.0, 0.0);
            let drawn = CGPoint::new(along.x + nudge.x, 0.0);
            let presented = Presented {
                drawn: [(group, drawn)].into_iter().collect(),
                along: [(group, along)].into_iter().collect(),
            };
            let rel = |plan: &FlightPlan, window| match plan.member(window) {
                Some(Member::Rigid { rel, .. }) => rel,
                other => panic!("{other:?}"),
            };
            let before = overlay_of(rel(&current, wid(2)), drawn);

            let elsewhere = at(1500.0, 860.0);
            let pass = reflow_plan(
                &[
                    (wid(1), a, rect(a.origin.x + v.x, 32.0, 860.0, 1081.0), false),
                    (wid(2), b, elsewhere, false),
                ],
                DISPLAY,
            );
            let (merged, delta) = merge_plans(&current, &pass, None, &presented, None, DISPLAY);
            let NewGroup { install, leaving, .. } = delta.new_groups[0];
            assert_eq!(leaving, Some(group));
            let key = key_of(&merged, wid(2)).unwrap();
            assert_ne!(key, group);
            assert_eq!(overlay_of(rel(&merged, wid(2)), install), before, "no jump");
            assert!(dest(&merged, wid(2)).unwrap().same_as(elsewhere));
        }
    }
}
