//! The flight engine: the actor that runs every transition. It owns the renderer
//! (`crate::animation::platform::overlay`) and is a client of the pictures service
//! (`crate::pictures::platform::service`) for what it draws. Runs on the main thread because Core
//! Animation requires it.
//!
//! Design in `src/transitions/docs/coordination.md`; how a flight moves in
//! `src/animation/docs/animation-smoothness.md`; measurements in `src/pictures/docs/capture-research.md` and
//! `src/animation/docs/capture-overlay-research.md`.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_foundation::MainThreadMarker;
use rini_core::ids::{WindowId, WindowServerId};
use rini_geometry::{SameAs, to_overlay_space};
use rini_runloop::channel;
use rini_runloop::run_loop::RepeatingTimer;
use tracing::{debug, warn};

use crate::animation::domain::motion::accents::{NudgeAdmission, NudgePlaying, nudge_admission};
use crate::animation::domain::motion::bounce::Bounce;
use crate::animation::domain::motion::nudge::{NUDGE_STRETCH, flight_curve};
use crate::animation::domain::motion::plan::{self, entrance_from};
use crate::animation::domain::motion::surface::{SurfaceGeometry, pan_travel};
use crate::animation::platform::overlay::{OverlayTile, TileOverlay};
use crate::pictures::domain::schedule::{
    Ask, Display, Hold, MAX_SYNC_ENTRANCE_CAPTURES, Priority, REVEAL_CHASE_ATTEMPTS,
    REVEAL_CHASE_INTERVAL,
};
use crate::pictures::domain::settle::chase_settled;
use crate::pictures::domain::stand_in::picture_or_stand_in;
use crate::pictures::domain::target::{PictureTarget, synthetic_window_id};
use crate::pictures::platform::picture::Picture;
use crate::pictures::platform::service::Pictures;
use crate::pictures::platform::skylight::{capture_via_framed, is_wholly_on_a_display};
use crate::transitions::domain::admission::*;
use crate::transitions::domain::flight::*;
use crate::transitions::domain::request::{AnimationRequest, SurfaceWindow};
use crate::transitions::domain::timing::*;
use crate::transitions::domain::travel::{TilePath, is_moving, tile_path};

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
    /// Slide every currently visible window in from an offset, purely to evaluate animation quality
    /// by eye. Does not touch any real window, so it is safe to fire at any time.
    DebugSlide {
        dx: f64,
        dy: f64,
        duration: Duration,
    },
    /// Move the whole strip surface by one travel, as one rigid group; a leaving window animates
    /// off screen while its real frame parks, and the columns a move swapped cross the surface from
    /// their old slots, but for the one passed when the strip steps, which is held on it. See
    /// "Strip movements" in `src/transitions/docs/coordination.md`.
    AnimateSurface {
        windows: Vec<SurfaceWindow>,
        from_offset: CGPoint,
        to_offset: CGPoint,
        /// Real screen frames to apply once the overlay is covering them.
        final_frames: Vec<(WindowId, CGRect)>,
        /// The window that will hold focus once this settles, drawn in front of the rest.
        focus: Option<WindowId>,
        duration: Duration,
        /// The whole strip's step away from where a moved window went and back; drawn only.
        nudge: Option<CGPoint>,
    },
    /// Nudge the strip surface by `overshoot` and bring it back; real windows stay put. Rides an
    /// in-flight movement additively. See "Edge bounce" in `src/transitions/docs/coordination.md`.
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
    /// A background capture has landed. Posted by the pictures service.
    Pictures(crate::pictures::event::Event),
    /// A framed recapture has landed. `settled` (`chase_settled`) means the window has repainted;
    /// only settled pictures may satisfy a reveal hold or replace a resizing tile's picture.
    PictureReady {
        window: WindowId,
        snapshot: Picture,
        settled: bool,
    },
}

/// Called with real-window frames to apply while the overlay covers them.
pub type PlaceFrames = Box<dyn Fn(Vec<(WindowId, CGRect)>)>;

/// Called with `true` when an overlay goes up and `false` once flying has settled; see
/// `domain::flight::FlightReport`.
pub type OnFlight = Box<dyn Fn(bool)>;

pub type Sender = channel::Sender<Event>;
pub type Receiver = channel::Receiver<Event>;

/// The focus refresh's capture targets, one per wanted window that has a tile, and the windows covered.
fn refresh_requests(
    tiles: &[(WindowId, WindowServerId, CGSize)],
    wanted: &[WindowId],
) -> (Vec<WindowId>, Vec<PictureTarget>) {
    let requests: Vec<PictureTarget> = wanted
        .iter()
        .filter_map(|window| tiles.iter().find(|(w, _, _)| w == window))
        .map(|&(window, server_id, size)| PictureTarget { window, server_id, size })
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
    /// When to stop waiting for reveal pixels and fly the stand-in (`HOLD_CAP` after the hold began).
    hold_deadline: Option<Instant>,
    /// The recapture of the focus change's two ends. See `refresh_destination_among`.
    refresh: FocusRefresh,
    /// The window gaining focus, from the latest pass that named one; its group is banded in front.
    focus: Option<WindowId>,
    /// The flight as rigid pieces: what `install` composed and `fly` animates.
    /// See "The overlay engine" in `src/animation/docs/animation-smoothness.md`.
    plan: plan::FlightPlan,
    /// A move's nudge waiting for the flight to move, with the move's plain duration
    /// (`NudgeAdmission::Wait`).
    nudge: Option<(CGPoint, Duration)>,
    /// Dropped when the animation ends, which invalidates the timer.
    _clock: Option<RepeatingTimer>,
}

/// The thumbprint of a snapshot's bitmap or region; `None` for a surface, which cannot be compared.
fn bitmap_thumbprint(snapshot: &Picture) -> Option<Vec<u8>> {
    crate::pictures::platform::bitmap::thumbprint(&*snapshot.image.bitmap()?)
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
/// border in the floating container with it. See "Border companions" in
/// `src/transitions/docs/coordination.md`.
fn border_tile(
    anchor: &OverlayTile,
    real: CGRect,
    border: (WindowId, CGRect),
    snapshot: Picture,
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
        radius: None,
    }
}

/// The stand-ins a flight that does not hold chases the real pictures of (`chases_stand_in`), with
/// the size a picture must fit. A window lands where its real frame goes, `final_frames` on
/// `display`, which for a column a move holds on the strip is not where its tile is drawn going;
/// one with no frame to apply stays where its tile lands. The reveal swap puts the picture on the
/// same tile mid-flight.
fn stand_in_chase(
    tiles: &[OverlayTile],
    final_frames: &[(WindowId, CGRect)],
    display: CGRect,
) -> Vec<(WindowId, CGSize)> {
    let viewport = to_overlay_space(display, display);
    tiles
        .iter()
        .filter(|tile| {
            let lands = final_frames
                .iter()
                .find(|(window, _)| *window == tile.window)
                .map_or(tile.to, |(_, frame)| to_overlay_space(*frame, display));
            tile.snapshot.source
                == crate::pictures::platform::picture::Route::StandIn
                && chases_stand_in(lands, viewport)
        })
        .map(|tile| (tile.window, tile.to.size))
        .collect()
}

/// A move's nudge arriving at a flight that does not move yet, kept with the move's plain duration
/// until it does (`NudgeAdmission::Wait`); `None` when it is skipped.
fn nudge_waiting(
    nudge: Option<CGPoint>,
    duration: Duration,
    playing: &NudgePlaying,
    now: Instant,
) -> Option<(CGPoint, Duration)> {
    let nudge = nudge?;
    match nudge_admission(playing, now, false) {
        NudgeAdmission::Wait => Some((nudge, duration)),
        admission => {
            admitted(nudge, admission);
            None
        }
    }
}

/// The waiting nudge as its flight starts moving, with the stretched duration it plays over; `None`
/// when it is skipped.
fn starting_nudge(
    waiting: Option<(CGPoint, Duration)>,
    playing: &NudgePlaying,
    now: Instant,
) -> Option<(CGPoint, Duration)> {
    let (nudge, plain) = waiting?;
    let nudge = admitted(nudge, nudge_admission(playing, now, true))?;
    Some((nudge, plain.mul_f64(NUDGE_STRETCH)))
}

/// The nudge that starts now, or `None`, logged when it is skipped.
fn admitted(nudge: CGPoint, admission: NudgeAdmission) -> Option<CGPoint> {
    match admission {
        NudgeAdmission::Start => Some(nudge),
        NudgeAdmission::Wait => None,
        NudgeAdmission::Skip => {
            debug!("move nudge skipped: one is still playing");
            None
        }
    }
}

/// Steps the whole strip out by `offset` and back over `duration`, and holds the flight's clock and
/// lift for it to come home. See "The move flight" in `src/animation/docs/animation-smoothness.md`.
fn nudge_on(
    overlay: &mut TileOverlay,
    running: &mut RunningAnimation,
    playing: &mut NudgePlaying,
    offset: CGPoint,
    duration: Duration,
) {
    playing.start(Instant::now(), duration);
    running.duration = clock_for_bounce(running.started, running.duration, duration);
    overlay.nudge(offset, duration);
    debug!(offset = format!("{:.0},{:.0}", offset.x, offset.y), "move nudge");
}

/// The bounce a push at `now` plays: from rest with none playing, else pushed from where the one
/// playing is drawn.
fn next_bounce(
    playing: Option<(Instant, Bounce)>,
    now: Instant,
    overshoot: CGPoint,
    duration: Duration,
) -> Bounce {
    match playing {
        Some((began, bounce)) => {
            bounce.pushed(now.saturating_duration_since(began), overshoot, duration)
        }
        None => Bounce::from_rest(overshoot, duration),
    }
}

/// The capture work a lift leaves for the quiet period.
#[derive(Default)]
struct AfterFlight {
    targets: Vec<PictureTarget>,
}

/// The tile for a reserved entrance whose picture has landed: frontmost and focused, since a
/// window is raised on open and about to hold focus.
fn entrance_tile(entrance: &PendingEntrance, snapshot: &Picture) -> OverlayTile {
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
        radius: None,
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
            group: crate::windows::domain::raise_order::StackGroup::of(t.floating),
            server_order: t.server_order,
        })
        .collect()
}

/// Depth for every tile, banded by `z_group::stack`; companions keep their window's depth.
/// The reactor's regroup matches it. See "The overlay engine" in `src/animation/docs/animation-smoothness.md`.
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

    /// `progress` as `should_swap_mid_flight` wants it: `None` until the flight starts moving.
    fn progress_if_started(&self) -> Option<f64> {
        self.started.map(|_| self.progress())
    }

    /// Whether a focus refresh pass is due at `progress`; takes it when it is. Only a moving flight
    /// refreshes: a hold has nothing in flight to cut a picture onto.
    fn take_refresh(&mut self, progress: f64) -> bool {
        self.refresh.take_pass(progress, self.started.is_some())
    }

    /// What this flight asks for the windows a pass has no picture of.
    fn missing_pictures(&self) -> Priority {
        missing_pictures_priority(self.started.is_some(), !self.awaiting.is_empty())
    }

    /// What `window` is doing in this flight when a picture of it lands, for `should_swap_mid_flight`.
    fn tile_state(&self, window: WindowId, snapshot: &Picture) -> TileState {
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
        if crate::pictures::domain::coverage::outgrows(
            tile.snapshot.coverage.covered,
            tile.to.size,
        ) {
            return TileState::Reveal { fits };
        }
        let resizing =
            rini_geometry::stretch::is_a_resize(tile.from.size, tile.to.size);
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
            self.hold_deadline = Some(now + HOLD_CAP);
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
    /// destination, for entrances too. See "The reservation fallback" in `src/transitions/docs/coordination.md`.
    fn claim(&mut self, window: WindowId, snapshot: &Picture) -> Option<Claimed> {
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
        snapshot: &Picture,
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

pub struct FlightEngine {
    rx: Receiver,
    /// Timers post back into this actor's own queue through it.
    tx: Sender,
    mtm: MainThreadMarker,
    overlay: Option<TileOverlay>,
    /// Every picture the flight draws, and the background captures that fill it.
    pictures: Pictures,
    display: Option<(CGRect, f64)>,
    /// Which display the overlay is on, for the pictures of it.
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
    last_animated: Vec<PictureTarget>,
    /// The focus the previous flight landed on, for `refresh_targets`. Kept across a flight that
    /// names no focus.
    last_focus: Option<WindowId>,
    /// Whether a bar under the overlay is pictured; see `Event::SetDisplay`.
    picture_bar: bool,
    /// Places the real windows once the overlay covers them. Supplied by the owner.
    place_frames: Option<PlaceFrames>,
    on_flight: Option<OnFlight>,
    flight_report: FlightReport,
    /// When the move nudge playing now comes home.
    nudge_playing: NudgePlaying,
    /// The edge bounce drawn on the containers now and when it began. None at the lift, so every
    /// flight starts with none, and when frame zero is installed again.
    bounce: Option<(Instant, Bounce)>,
}

impl FlightEngine {
    /// `pictures` tells this engine what lands through its sink, as `Event::Pictures`.
    pub fn new(rx: Receiver, tx: Sender, mtm: MainThreadMarker, pictures: Pictures) -> Self {
        Self {
            rx,
            tx,
            mtm,
            overlay: None,
            pictures,
            display: None,
            display_id: None,
            top_band: 0.0,
            running: None,
            coalesce: None,
            quiet: None,
            after_flight: None,
            last_animated: Vec::new(),
            last_focus: None,
            picture_bar: true,
            place_frames: None,
            on_flight: None,
            flight_report: FlightReport::default(),
            nudge_playing: NudgePlaying::default(),
            bounce: None,
        }
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
            Event::DebugSlide { dx, dy, duration } => self.debug_slide(dx, dy, duration),
            Event::Tick => self.step(),
            Event::StartMoving => self.start_moving(),
            Event::Quiet => self.after_flight_captures(),
            Event::Pictures(landed) => self.pictures_landed(landed),
            Event::PictureReady { window, snapshot, settled } => {
                self.picture_ready(window, snapshot, settled)
            }
        }
    }

    /// Takes a background capture the pictures service landed: a picture the cache took is offered
    /// to the running flight.
    fn pictures_landed(&mut self, landed: crate::pictures::event::Event) {
        let (window, picture) = match landed {
            crate::pictures::event::Event::Taken { window, picture } => (window, picture),
            // Asked for by watches and asks of windows on screen, which the engine makes none of.
            crate::pictures::event::Event::Delivered { .. } | crate::pictures::event::Event::Lost { .. } => {
                return;
            }
        };
        // Background captures are never settled: the service knows sizes, not paint states.
        if picture.is_usable() {
            self.offer_mid_flight(window, &picture, false);
        }
    }

    /// Offers a picture the cache just took to the running flight per `should_swap_mid_flight`. One
    /// the cache refused is never offered: a flight would draw what the next one will not.
    fn offer_mid_flight(&mut self, window: WindowId, snapshot: &Picture, settled: bool) {
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

    /// The overlay's geometry. The pictures of the display are the service's, told by the reactor.
    fn set_display(&mut self, id: u32, frame: CGRect, scale: f64, picture_bar: bool, top_band: f64) {
        self.picture_bar = picture_bar;
        self.display = Some((frame, scale));
        self.display_id = Some(id);
        self.top_band = top_band;
        if let Some(overlay) = self.overlay.as_mut() {
            overlay.set_frame(frame, top_band, scale);
        }
    }

    /// The display the overlay is on, as the pictures of it are keyed.
    fn picture_display(&self) -> Option<Display> {
        let ((frame, scale), id) = (self.display?, self.display_id?);
        Some(Display { id, frame, scale })
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
    /// ScreenCaptureKit where it is clipped, which a framed capture returns only a sliver of.
    /// See "Mid-flight passes" in `src/transitions/docs/coordination.md`.
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
        let (framed, clipped): (Vec<PictureTarget>, Vec<PictureTarget>) = requests
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
        if !clipped.is_empty() {
            self.pictures.ask(Ask {
                priority: Priority::Focus,
                targets: clipped,
            });
        }
    }

    /// Framed captures of `windows` on a plain thread, each turned into what `event` sends back. Never
    /// on the service's completion queue, which must not make capture calls (see `screencapturekit`).
    fn capture_framed(
        &self,
        name: &str,
        windows: Vec<WindowId>,
        event: fn(WindowId, Picture) -> Option<Event>,
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
                    let Some(snapshot) = capture_via_framed(server_id, scale) else {
                        continue;
                    };
                    if let Some(event) = event(window, snapshot) {
                        _ = tx.send(event);
                    }
                }
            })
            .ok();
    }

    /// Takes a framed recapture: a chase's reveal, or the destination refresh.
    fn picture_ready(&mut self, window: WindowId, snapshot: Picture, settled: bool) {
        if self.pictures.insert(window, snapshot.clone()) {
            self.offer_mid_flight(window, &snapshot, settled);
        }
    }

    /// How much of the running flight is left, in wall-clock time.
    fn remaining_flight(&self) -> Option<Duration> {
        self.running.as_ref().map(RunningAnimation::remaining)
    }

    /// Chases the first settled picture for a holding grow or entrance, one thread per window.
    /// See "A grow holds, then reveals" in `src/transitions/docs/coordination.md`.
    fn chase_reveal_pictures(&self, awaiting: &[(WindowId, CGSize)]) {
        let scale = self.display.map(|(_, scale)| scale).unwrap_or(2.0);
        for (window, size) in awaiting.iter().copied() {
            let server_id = WindowServerId::from(window);
            let tx = self.tx.clone();
            // The picture the tile flies from; a capture that no longer renders like it is the
            // app's repaint. An entrance has none.
            let pre_resize = self.pictures.get(window).as_ref().and_then(bitmap_thumbprint);
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
                        let frame_fits = crate::pictures::domain::coverage::fits_frame(
                            (info.frame.size.width, info.frame.size.height),
                            (size.width, size.height),
                        );
                        if !frame_fits {
                            last_print = None;
                            continue;
                        }
                        let Some(snapshot) =
                            crate::pictures::platform::skylight::capture_via_framed(server_id, scale)
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
                        "reveal chase gave up; the hold deadline will fly the stand-in"
                    );
                })
                .ok();
        }
    }

    /// Takes a landed picture for a window a holding flight is waiting on. Returns whether the
    /// hold claimed it.
    fn claim_reveal(&mut self, window: WindowId, snapshot: &Picture) -> bool {
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
        let Self { overlay, running, bounce, .. } = self;
        let Some(running) = running.as_mut() else { return };
        let mut rebuilt = plan::plan_from_tiles(&running.tiles);
        // A tile only knows it resizes; the plan remembers which of those are entrances.
        let entrances: Vec<WindowId> = running.plan.entrances.iter().map(|(w, _, _)| *w).collect();
        rebuilt.mark_entrances(&entrances);
        running.plan = plan::FlightPlan::from(rebuilt);
        if let Some(overlay) = overlay.as_mut() {
            let banding = band_plan(&running.plan, &running.tiles, running.focus);
            overlay.install(&running.plan, &running.tiles, &banding);
            *bounce = None;
        }
    }

    /// Adds the tile for a reserved entrance whose first picture landed after the flight started
    /// moving. Returns whether the picture was taken.
    fn admit_entrance(&mut self, window: WindowId, snapshot: &Picture) -> bool {
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

    /// Tiles for the border windows tracing the animated windows; each anchor is the window's real
    /// frame and its tile. See "Border companions" in `src/transitions/docs/coordination.md`.
    fn companion_tiles(
        &mut self,
        display: CGRect,
        anchors: &[(CGRect, &OverlayTile)],
        exclude: &std::collections::HashSet<u32>,
        needs_capture: &mut Vec<PictureTarget>,
    ) -> (Vec<OverlayTile>, Vec<PictureTarget>) {
        if anchors.is_empty() {
            return (Vec::new(), Vec::new());
        }
        // Every window rini manages is excluded, not only the pass's: parked windows share a frame.
        let managed = managed_server_ids(
            exclude,
            self.pictures.windows().into_iter(),
            self.after_flight.iter().flat_map(|a| a.targets.iter()).map(|t| t.window),
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
            targets.push(PictureTarget {
                window,
                server_id,
                size: frame.size,
            });
            match self.pictures.usable(window) {
                Some(snapshot) => tiles.push(border_tile(anchor, real, (window, frame), snapshot)),
                None => needs_capture.push(PictureTarget {
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
            rini_geometry::stretch::is_a_resize(
                request.from.size,
                request.to.size,
            )
        });
        let apply_at = apply_frames_at(FlightKind::Layout, any_resize);

        let mut tiles = Vec::with_capacity(windows.len());
        let mut skipped = 0usize;
        let mut offscreen = 0usize;
        let mut needs_capture: Vec<PictureTarget> = Vec::new();
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
            // Any usable picture, whatever its shape; a wrong-shaped one is drawn cropped. See
            // "Resizes through the overlay" in `src/animation/docs/animation-smoothness.md`.
            let snapshot = self.pictures.usable(request.window);
            if snapshot.is_none() {
                // Asked for now so the next switch has pixels, even if this one does not.
                needs_capture.push(PictureTarget {
                    window: request.window,
                    server_id: request.server_id,
                    size: request.from.size,
                });
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
                || self.pictures.stand_in(request.to.size, scale),
            );
            match snapshot {
                Some(snapshot) => {
                    // A grow whose picture cannot cover the destination holds for the reveal.
                    if crate::pictures::domain::coverage::outgrows(
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
                        radius: self.pictures.knowledge(request.server_id).radius,
                    });
                    starts.push((request.window, start));
                    resolved.push((request.window, start, end, request.floating));
                }
                // No picture: almost always a window that just opened. See "A window that opens"
                // in `src/transitions/docs/coordination.md`.
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
                            capture_via_framed(request.server_id, scale)
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
                            self.pictures.insert(request.window, snapshot.clone());
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
                                radius: self.pictures.knowledge(request.server_id).radius,
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
                            awaiting.push(waiting);
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
        self.ask_missing(needs_capture);
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
            .map(|request| PictureTarget {
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
            self.warm_last_animated();
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
        nudge: Option<CGPoint>,
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
                hold_frames = running.extend_hold(&awaiting, in_flight, Instant::now());
                focus_before = running.focus;
                running.merge_pass(tiles, focus);
                if !in_flight
                    && let Some(waiting) =
                        nudge_waiting(nudge, duration, &self.nudge_playing, Instant::now())
                {
                    running.nudge = Some(waiting);
                }
            }
            if in_flight {
                // See "Mid-flight passes" in `src/transitions/docs/coordination.md`.
                let Self {
                    overlay,
                    running,
                    nudge_playing,
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
                let nudge = nudge
                    .and_then(|nudge| admitted(nudge, nudge_admission(nudge_playing, now, true)));
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
                if let (Some(nudge), Some(overlay)) = (nudge, overlay.as_mut()) {
                    nudge_on(overlay, running, nudge_playing, nudge, duration);
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
            self.warm_last_animated();
            return;
        }

        // A flight beginning inside the quiet period carries the owed captures to its own lift.
        self.quiet = None;
        let backdrop_drawn = self.overlay.as_ref().is_some_and(TileOverlay::backdrop_drawn);
        let dressing = self
            .picture_display()
            .map(|display| self.pictures.dressing(display, self.picture_bar, backdrop_drawn));
        let display_origin = self.display.map(|(frame, _)| frame.origin).unwrap_or_default();
        let Some(overlay) = self.ensure_overlay() else {
            self.request_frames(final_frames);
            return;
        };
        restack(&mut tiles, focus);
        if let Some(dressing) = &dressing {
            let strip = dressing.strip.map(|strip| {
                CGRect::new(
                    CGPoint::new(
                        strip.origin.x - display_origin.x,
                        strip.origin.y - display_origin.y,
                    ),
                    strip.size,
                )
            });
            log_dressing(label, dressing, strip);
            overlay.set_backdrop(dressing.desktop.as_ref());
            overlay.set_bar(dressing.bar.as_ref(), strip);
        }
        let plan = plan::FlightPlan::from(plan);
        overlay.install(&plan, &tiles, &band_plan(&plan, &tiles, focus));
        // Held before the overlay shows: a bar read off the framebuffer from here on has it in.
        self.pictures.hold(Hold::Transition);
        let Some(overlay) = self.overlay.as_mut() else { return };
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
            hold_deadline: holding.then(|| Instant::now() + HOLD_CAP),
            awaiting,
            refresh: FocusRefresh::default(),
            focus,
            plan,
            nudge: nudge_waiting(nudge, duration, &self.nudge_playing, Instant::now()),
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
    /// animation, with a container of its own for each column a move swapped that crosses it, and a
    /// move's `nudge` stepping every strip container. See "The move flight, staged" in
    /// `src/transitions/docs/coordination.md`.
    fn start_surface(
        &mut self,
        windows: Vec<SurfaceWindow>,
        from_offset: CGPoint,
        to_offset: CGPoint,
        final_frames: Vec<(WindowId, CGRect)>,
        focus: Option<WindowId>,
        duration: Duration,
        nudge: Option<CGPoint>,
    ) {
        // Remembered before anything can fail, so a window with no picture is still warmed.
        self.last_animated = windows
            .iter()
            .map(|w| PictureTarget {
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
        let mut needs_capture: Vec<PictureTarget> = Vec::new();
        let mut starts: Vec<(WindowId, CGRect)> = Vec::new();
        let display_frame = self.display.map(|(frame, _)| frame);
        let scale = self.display.map(|(_, scale)| scale).unwrap_or(2.0);
        for window in &windows {
            let (from, to) = window.travel(from_offset, to_offset);
            let cached = self.pictures.usable(window.window);
            // A known window parked off this display with no picture flies as the stand-in, as on
            // the per-window path; see `stands_in`.
            let snapshot = match display_frame {
                Some(display) => picture_or_stand_in(
                    cached,
                    || real_frames.get(&window.server_id).copied(),
                    display,
                    || self.pictures.stand_in(window.frame.size, scale),
                ),
                None => cached,
            };
            match snapshot {
                Some(snapshot) => {
                    // A wrong-shaped picture is stretched rather than dropped. See "A window that
                    // was resized keeps a usable picture" in `src/pictures/docs/capture-research.md`.
                    if snapshot.source
                        == crate::pictures::platform::picture::Route::StandIn
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
                        radius: self.pictures.knowledge(window.server_id).radius,
                    });
                }
                // Still placed by `final_frames`, and warmed once the movement settles.
                None => missing += 1,
            }
        }
        restack(&mut tiles, focus);
        let chase = display_frame
            .map(|display| stand_in_chase(&tiles, &final_frames, display))
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
        self.ask_missing(needs_capture);
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

        // One rigid piece for the strip and one for each column a move swapped that crosses it;
        // companions are adopted by their own vectors.
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

    /// Pushes the strip surface out by `overshoot` and back, additively on a running flight or as
    /// the only motion of a no-travel one. Every push shows: one arriving while a bounce plays
    /// starts the next from where that one is drawn. See "Edge bounce" in
    /// `src/transitions/docs/coordination.md`.
    fn start_bounce(
        &mut self,
        windows: Vec<SurfaceWindow>,
        overshoot: CGPoint,
        final_frames: Vec<(WindowId, CGRect)>,
        focus: Option<WindowId>,
        duration: Duration,
    ) {
        let now = Instant::now();
        if self.running.is_none() {
            let at_rest = CGPoint::new(0.0, 0.0);
            self.start_surface(windows, at_rest, at_rest, final_frames, focus, duration, None);
        }
        let Self { overlay, running, bounce, .. } = self;
        let (Some(overlay), Some(running)) = (overlay.as_mut(), running.as_mut()) else {
            return;
        };
        let before = bounce.map(|(_, playing)| playing.overshoot);
        let next = next_bounce(*bounce, now, overshoot, duration);
        *bounce = Some((now, next));
        running.duration = clock_for_bounce(running.started, running.duration, next.duration);
        overlay.bounce(&next, before);
        debug!(
            overshoot = format!("{:.0},{:.0}", overshoot.x, overshoot.y),
            from = format!("{:.1},{:.1}", next.from.x, next.from.y),
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
                "reveal pixels did not arrive in time; flying with the stand-in"
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
        let nudge = starting_nudge(running.nudge.take(), &self.nudge_playing, now);
        // A flight carrying the nudge is stretched, and crosses on the curve that keeps the column
        // the move passed in the side the step opens.
        if let Some((_, stretched)) = nudge {
            running.duration = running.duration.max(stretched);
        }
        let curve = flight_curve(nudge.is_some());
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
            overlay.fly(flight, duration, curve);
        }
        if let Some(running) = self.running.as_mut() {
            running.tiles = tiles;
        }
        if let Some((nudge, stretched)) = nudge
            && let (Some(overlay), Some(running)) = (self.overlay.as_mut(), self.running.as_mut())
        {
            nudge_on(overlay, running, &mut self.nudge_playing, nudge, stretched);
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
    /// See "Real windows land before lift" in `src/transitions/docs/coordination.md`.
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

    /// Asks the pictures service for the windows a pass has no picture of: at frame zero if they can
    /// still serve this flight, else as warms for the next.
    fn ask_missing(&self, targets: Vec<PictureTarget>) {
        if targets.is_empty() {
            return;
        }
        let priority = self
            .running
            .as_ref()
            .map_or(Priority::FrameZero, RunningAnimation::missing_pictures);
        debug!(
            count = targets.len(),
            ?priority,
            "asking for pictures the flight has none of"
        );
        self.pictures.ask(Ask { priority, targets });
    }

    /// Warms the windows of the pass just composed, which is not flying.
    fn warm_last_animated(&mut self) {
        let targets = std::mem::take(&mut self.last_animated);
        if !targets.is_empty() {
            self.pictures.ask(Ask {
                priority: Priority::Warm,
                targets,
            });
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
                overlay.fly(&running.plan, Duration::ZERO, flight_curve(false));
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
        self.pictures.hold(Hold::Settling);
        let focus = self.running.take().and_then(|running| running.focus);
        if focus.is_some() {
            self.last_focus = focus;
        }
        self.bounce = None;
        self.coalesce = None;

        // The owed captures run after `SETTLE_BEFORE_CAPTURES`; a flight beginning first carries them over.
        let targets = std::mem::take(&mut self.last_animated);
        let after = self.after_flight.get_or_insert_with(AfterFlight::default);
        for target in targets {
            if !after.targets.iter().any(|t| t.window == target.window) {
                after.targets.push(target);
            }
        }
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
        self.pictures.hold(Hold::Released);
        let Some(after) = self.after_flight.take() else { return };
        if !after.targets.is_empty() {
            self.pictures.ask(Ask {
                priority: Priority::Warm,
                targets: after.targets,
            });
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

/// Records what the overlay was dressed with; a black backdrop is only diagnosable after the fact.
fn log_dressing(
    path: &str,
    dressing: &crate::pictures::platform::service::Dressing,
    strip: Option<CGRect>,
) {
    debug!(
        path,
        backdrop = dressing
            .desktop
            .as_ref()
            .map(|b| format!(
                "{:.0}x{:.0} {:?}",
                b.coverage.covered.0, b.coverage.covered.1, b.source
            ))
            .unwrap_or_else(|| "NONE".to_string()),
        bar = dressing
            .bar
            .as_ref()
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::animation::domain::motion::testing::{DISPLAY, Gen, RUNS, rect};
    use crate::pictures::platform::picture::test_picture;

    fn wid(idx: u32) -> WindowId {
        WindowId::new(7, idx)
    }

    /// A 300ms layout flight with nothing in it: at frame zero, or moving since `started`.
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
            focus: None,
            plan: plan::FlightPlan::empty(),
            nudge: None,
            _clock: None,
        }
    }

    /// A tile drawn from a picture the size of its destination.
    fn stacked(
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
            snapshot: test_picture(to.size),
            floating,
            server_order,
            depth: 0,
            companion: None,
            focused: false,
            radius: None,
        }
    }

    /// A flight holding for one entrance at `to`, composed the way `start` does it.
    fn holding_for(window: WindowId, to: CGRect, floating: bool) -> RunningAnimation {
        let mut flight = flight(None);
        let (entrance, waiting) = entrance_reservation(window, to, floating);
        flight.entrances.push(entrance);
        flight.awaiting.push(waiting);
        flight.frames_applied = true;
        flight
    }

    /// A SkyLight sliver: 40pt of an `size`-wide window, which `is_usable` rejects.
    fn clipped(size: CGSize) -> Picture {
        let mut snapshot = test_picture(size);
        snapshot.coverage.covered = (40.0, size.height);
        snapshot
    }

    /// The flight's clock: how far it is, and what is left of it.
    mod clock {
        use super::*;

        #[test]
        fn progress_is_complete_for_a_zero_length_animation() {
            // Zero durations resolve at once, and the division is guarded.
            let mut running = flight(Some(Instant::now()));
            running.duration = Duration::ZERO;
            assert_eq!(running.progress(), 1.0);
            assert!(running.is_done());
        }

        #[test]
        fn progress_starts_near_zero_and_is_clamped_to_one() {
            let mut running = flight(Some(Instant::now()));
            running.duration = Duration::from_millis(180);
            assert!(running.progress() < 0.2, "just started");

            let mut finished = flight(Some(Instant::now() - Duration::from_secs(5)));
            finished.duration = Duration::from_millis(180);
            // Clamped, or the easing overshoots when a frame arrives late.
            assert_eq!(finished.progress(), 1.0);
            assert!(finished.is_done());
        }

        #[test]
        fn a_late_entrance_ends_no_later_than_the_flight() {
            let duration = Duration::from_millis(300);
            let running = flight(Some(Instant::now() - duration.mul_f64(0.6)));
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
    }

    /// The frames a flight owes the reactor: at frame zero when it holds, at the apply point
    /// otherwise, and again when a merge leaves them stale.
    mod frames {
        use super::*;
        use crate::animation::domain::motion::surface::surface_travel;
        use crate::pictures::domain::coverage::needs_capture;

        fn column(i: f64) -> CGRect {
            rect(4.0 + i * 863.0, 32.0, 859.0, 1081.0)
        }

        /// A holding flight sends every frame at frame zero, the newcomer's slot included, so
        /// nothing is owed at the apply point.
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
            let awaiting: Vec<(WindowId, CGSize)> = vec![waiting];
            let now = Instant::now();
            let requested = running.extend_hold(&awaiting, false, now);
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

        #[test]
        fn a_held_merge_re_requests_the_flights_frames() {
            let mut rng = Gen(36);
            let mut checked = 0;
            for _ in 0..RUNS {
                let count = rng.below(3) as u32 + 1;
                let frames: Vec<(WindowId, CGRect)> =
                    (1..=count).map(|i| (wid(i), rng.on_screen())).collect();
                let mut flight = flight(None);
                flight.final_frames = frames.clone();
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
                let requested = flight.extend_hold(&incoming, false, now);
                assert_eq!(requested, Some(frames.clone()));
                assert!(flight.frames_applied);
                assert_eq!(flight.hold_deadline, Some(now + HOLD_CAP));
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

        #[test]
        fn a_hold_never_stops_a_flight_in_motion() {
            let now = Instant::now();
            let mut flight = flight(None);
            flight.final_frames = vec![(wid(1), rect(4.0, 32.0, 859.0, 1081.0))];
            flight.started = Some(now);
            let grow = [(wid(1), CGSize::new(1720.0, 1081.0))];
            assert_eq!(flight.extend_hold(&grow, true, now), None);
            assert!(!flight.frames_applied);
            assert!(flight.awaiting.is_empty());
            assert!(flight.hold_deadline.is_none());
            flight.started = None;
            assert_eq!(flight.extend_hold(&[], false, now), None);
            assert!(!flight.frames_applied);
        }

        #[test]
        fn a_grow_still_enters_awaiting() {
            use crate::pictures::domain::coverage::outgrows;
            let a = column(0.0);
            let grown = rect(a.origin.x, a.origin.y, a.size.width + 600.0, a.size.height);
            let snapshot = test_picture(a.size);
            assert!(outgrows(snapshot.coverage.covered, grown.size));
            let mut running = flight(None);
            running.final_frames = vec![(wid(1), grown)];
            running.duration = Duration::from_millis(350);
            running.apply_at = 0.0;
            let frames = running.extend_hold(&[(wid(1), grown.size)], false, Instant::now());
            assert_eq!(running.awaiting, vec![(wid(1), grown.size)]);
            assert!(running.hold_deadline.is_some());
            assert_eq!(
                frames,
                Some(vec![(wid(1), grown)]),
                "the held frames go out under the overlay"
            );
        }

        #[test]
        fn an_untiled_frame_change_in_flight_clears_frames_applied() {
            let slot = rect(4.0, 32.0, 859.0, 1081.0);
            let park_a = rect(1727.0, 1116.0, 859.0, 1081.0);
            let park_b = rect(-858.0, 1116.0, 859.0, 1081.0);
            let mut running = flight(Some(Instant::now()));
            running.tiles.push(stacked(wid(1), rect(867.0, 32.0, 859.0, 1081.0), slot, None, false));
            running.final_frames = vec![(wid(1), slot), (wid(2), park_a)];
            running.frames_applied = true;

            // The same tile again, the untiled window to the other park.
            let frames_changed = merge_final_frames(
                &mut running.final_frames,
                vec![(wid(1), slot), (wid(2), park_b)],
            );
            let outcomes = running.merge_pass(vec![stacked(wid(1), slot, slot, None, false)], None);
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
            let outcomes = running.merge_pass(vec![stacked(wid(1), slot, slot, None, false)], None);
            let changed = outcomes.iter().any(|(_, o)| *o != Admitted::Redundant);
            running.absorb_in_flight_change(changed, frames_changed);
            assert!(running.frames_applied, "a redundant pass cleared frames_applied");
        }

        #[test]
        fn a_strip_window_on_this_display_with_no_usable_picture_is_placed_but_not_drawn() {
            let mut rng = Gen(95);
            for _ in 0..RUNS {
                let slot = rng.on_screen();
                let mut cache: crate::pictures::domain::cache::PictureCache<Picture> =
                    crate::pictures::domain::cache::PictureCache::new();
                cache.insert(wid(2), clipped(slot.size));
                assert!(cache.usable(wid(1)).is_none(), "seed 95: never captured");
                assert!(cache.usable(wid(2)).is_none(), "seed 95: a sliver");
                assert!(cache.usable(wid(3)).is_none());
                cache.insert(wid(3), test_picture(slot.size));
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
                assert!(!needs_capture(Some(test_picture(slot.size).coverage), size));
            }
        }
    }

    /// A flight holding at frame zero takes the pictures it waits for, and only ones that fit.
    mod claims {
        use super::*;
        use crate::animation::domain::motion::surface::surface_travel;
        use crate::pictures::domain::coverage::outgrows;

        #[test]
        fn an_entrance_needs_the_fit_like_a_grow() {
            let slot = rect(867.0, 32.0, 859.0, 1081.0);
            let newcomer = wid(51462);
            let mut running = flight(None);
            let (entrance, waiting) = entrance_reservation(newcomer, slot, false);
            running.entrances.push(entrance);
            running.awaiting.push(waiting);
            let spawn = test_picture(CGSize::new(572.0, 540.0));
            assert_eq!(
                running.claim(newcomer, &spawn),
                None,
                "a spawn-size picture is refused"
            );
            assert_eq!(running.entrances.len(), 1);
            assert_eq!(running.awaiting.len(), 1);
            assert_eq!(
                running.claim(newcomer, &test_picture(slot.size)),
                Some(Claimed::Released),
                "a slot-size picture is the entrance"
            );
            assert!(running.entrances.is_empty() && running.awaiting.is_empty());
            let tile = running.tiles.iter().find(|t| t.window == newcomer).expect("tile");
            assert_eq!((tile.from, tile.to), (entrance_from(slot), slot));
        }

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
                let snapshot = test_picture(small.size);
                assert!(outgrows(snapshot.coverage.covered, to.size), "seed 91: a grow");
                // `start`: the outgrown picture puts the window in `awaiting` at its destination size.
                let awaiting = vec![(wid(1), to.size)];
                let (holding, chase, _) = frame_zero_work(&awaiting, &[], &[], &[]);
                assert!(holding, "seed 91");
                assert_eq!(chase, awaiting, "seed 91");

                let limit = HOLD_CAP;
                let now = Instant::now();
                let mut running = flight(None);
                let mut tile = stacked(wid(1), small, to, Some(0), false);
                tile.snapshot = snapshot.clone();
                running.tiles.push(tile);
                running.awaiting = awaiting.clone();
                running.frames_applied = holding;
                running.hold_deadline = Some(now + limit);
                assert!(
                    running.started.is_none() && !running.awaiting.is_empty(),
                    "seed 91: holding"
                );
                assert_eq!(
                    hold_wait(running.hold_deadline, now),
                    Some(limit.max(Duration::from_millis(10))),
                    "seed 91: waits out the hold"
                );
                assert_eq!(
                    hold_wait(running.hold_deadline, now + limit),
                    None,
                    "seed 91: at the deadline the stand-in flies"
                );
                assert_eq!(
                    running.claim(wid(1), &snapshot),
                    None,
                    "seed 91: too small to claim"
                );
                assert_eq!(running.awaiting, awaiting, "seed 91: the hold stands");
                assert_eq!(
                    running.claim(wid(1), &test_picture(to.size)),
                    Some(Claimed::Released),
                    "seed 91: the reveal is claimed"
                );
                assert!(
                    running.tiles[0].snapshot.fits(to.size),
                    "seed 91: drawn from the reveal"
                );
                assert!(
                    running.started.is_none() && running.awaiting.is_empty(),
                    "seed 91: released, not moving"
                );
            }
            assert_eq!(hold_wait(None, Instant::now()), None, "no deadline, no wait");
        }

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
                assert!(
                    running.started.is_none() && running.awaiting.is_empty(),
                    "seed 96: at frame zero"
                );
                assert!(!running.frames_applied, "seed 96: nothing applied at frame zero");
                assert_eq!(hold_wait(running.hold_deadline, Instant::now()), None, "seed 96");
                assert_eq!(
                    running.claim(wid(1), &test_picture(CGSize::new(859.0, 1081.0))),
                    None
                );
            }
        }

        /// A parked window with no picture flies as a stand-in tile — no hole — held for like a grow,
        /// and its real picture replaces the stand-in on the SAME tile when it lands at frame zero.
        #[test]
        fn a_stand_in_takes_its_real_picture_on_the_same_tile() {
            use crate::pictures::platform::picture::{stand_in_picture, test_bitmap};
            let parked = rect(-1720.0, 32.0, 859.0, 1081.0);
            let slot = rect(4.0, 32.0, 859.0, 1081.0);
            let mut flight = flight(None);
            let mut tile = stacked(wid(3), parked, slot, Some(2), false);
            tile.snapshot = stand_in_picture(slot.size, test_bitmap());
            flight.tiles.push(tile);
            flight.awaiting.push((wid(3), slot.size));

            assert!(
                matches!(
                    flight.tile_state(wid(3), &test_picture(slot.size)),
                    TileState::Reveal { fits: true }
                ),
                "held for like a grow"
            );
            let claimed = flight.claim(wid(3), &test_picture(slot.size));

            assert_eq!(claimed, Some(Claimed::Released));
            let tile = flight.tiles.iter().find(|t| t.window == wid(3)).expect("still one tile");
            assert_ne!(
                tile.snapshot.source,
                crate::pictures::platform::picture::Route::StandIn,
                "the stand-in is gone"
            );
            assert_eq!((tile.from, tile.to), (parked, slot), "and it travels as it did");
            assert!(
                flight.entrances.is_empty(),
                "never a newcomer's zero-width entrance"
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

            let claimed = flight.claim(wid(2), &test_picture(slot.size));

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
                flight.claim(wid(2), &test_picture(slot.size)),
                Some(Claimed::Released)
            );
            assert!(flight.awaiting.is_empty());
            assert_eq!(flight.tiles.len(), 1);
            assert_eq!(
                flight.claim(wid(2), &test_picture(slot.size)),
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
                flight.claim(wid(2), &test_picture(CGSize::new(200.0, 200.0))),
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
            assert_eq!(flight.claim(wid(2), &test_picture(slot.size)), None);
            assert!(flight.tiles.is_empty());
        }

        /// A grow's reveal picture lands on its own tile.
        #[test]
        fn a_grows_reveal_picture_replaces_its_tiles_snapshot() {
            let small = rect(4.0, 32.0, 400.0, 1081.0);
            let big = rect(4.0, 32.0, 1720.0, 1081.0);
            let mut flight = flight(None);
            let mut tile = stacked(wid(1), small, big, Some(0), false);
            tile.snapshot = test_picture(small.size);
            flight.tiles.push(tile);
            flight.awaiting.push((wid(1), big.size));
            assert_eq!(
                flight.claim(wid(1), &test_picture(big.size)),
                Some(Claimed::Released)
            );
            assert!(flight.tiles[0].snapshot.fits(big.size));
            assert_eq!(flight.tiles.len(), 1);
        }

        #[test]
        fn a_spawn_entrances_early_chase_picture_is_taken_without_a_release() {
            let slot = rect(867.0, 32.0, 859.0, 1081.0);
            let spawn = rect(300.0, 200.0, 400.0, 300.0);
            let mut running = flight(None);
            running.tiles = vec![stacked(wid(9), spawn, slot, Some(0), false)];
            running.final_frames = vec![(wid(9), slot)];
            running.duration = Duration::from_millis(350);
            running.apply_at = 0.0;
            running.tiles[0].snapshot = test_picture(spawn.size);
            assert_eq!(
                running.claim(wid(9), &test_picture(spawn.size)),
                None,
                "still spawn-sized"
            );
            assert_eq!(
                running.claim(wid(9), &test_picture(slot.size)),
                Some(Claimed::Refreshed)
            );
            assert!(running.tiles[0].snapshot.fits(slot.size));
            assert_eq!(
                running.claim(wid(77), &test_picture(slot.size)),
                None,
                "no such tile"
            );
        }
    }

    /// An entrance whose picture lands once the flight moves joins it for what is left.
    mod late_entrances {
        use super::*;

        /// Before the flight starts, `admit` does nothing and leaves the reservation to `claim`.
        #[test]
        fn a_late_entrance_travels_for_the_remaining_flight() {
            let slot = rect(867.0, 32.0, 859.0, 1081.0);
            let mut flight = holding_for(wid(2), slot, false);
            assert!(
                flight.admit(wid(2), &test_picture(slot.size)).is_none(),
                "still holding"
            );
            assert_eq!(flight.entrances.len(), 1);

            // The deadline passed: `start_moving` flew with the stand-in and cleared the hold.
            flight.awaiting.clear();
            flight.started = Some(Instant::now() - flight.duration.mul_f64(0.6));
            let remaining = flight.remaining();
            let (tile, travel) = flight.admit(wid(2), &test_picture(slot.size)).expect("admitted");
            assert_eq!(tile.from, entrance_from(slot));
            assert_eq!(tile.to, slot);
            assert!(travel <= remaining, "{travel:?} outlives {remaining:?}");
            assert!(
                travel < flight.duration.mul_f64(0.5),
                "not the full duration: {travel:?}"
            );
            assert!(flight.entrances.is_empty());
            assert!(
                flight.admit(wid(2), &test_picture(slot.size)).is_none(),
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
                let (_, admitted) = flight.admit(wid(2), &test_picture(to.size)).unwrap();
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
    }

    /// What a tile is doing when a picture of its window lands, and what that picture may change.
    mod landings {
        use super::*;
        use crate::animation::domain::motion::plan::{FlightPlan, Member, ReflowPlan};
        use crate::pictures::domain::coverage::should_replace;


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

        #[test]
        fn tile_state_ranks_hold_over_refresh_over_tile() {
            let slot = rect(4.0, 32.0, 859.0, 1081.0);
            let parked = rect(-1720.0, 32.0, 859.0, 1081.0);
            let grown = rect(4.0, 32.0, 1147.0, 1081.0);
            let picture = test_picture(slot.size);
            let mut running = flight(None);
            running.tiles.push(stacked(wid(1), parked, slot, None, false));
            running.tiles.push(stacked(wid(2), slot, grown, None, false));
            running.tiles.push(stacked(wid(3), parked, slot, None, false));
            running.awaiting.push((wid(2), grown.size));
            running.refresh.ask(vec![wid(3)], Instant::now());
            let (entrance, waiting) = entrance_reservation(wid(4), slot, false);
            running.entrances.push(entrance);
            running.awaiting.push(waiting);

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
                running.tile_state(wid(2), &test_picture(grown.size)),
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
                running.tile_state(wid(2), &test_picture(grown.size)),
                TileState::MovingRefreshTarget { fits: true, resizing: true }
            );
        }

        /// A stand-in tile is a reveal in waiting after a hold timeout and after a mid-flight join.
        #[test]
        fn a_stand_in_tile_takes_its_reveal_early() {
            let small = rect(4.0, 32.0, 859.0, 1081.0);
            let grown = rect(4.0, 32.0, 1147.0, 1081.0);
            let mut running = flight(Some(Instant::now()));
            let mut stand_in = stacked(wid(1), small, grown, None, false);
            stand_in.snapshot = test_picture(small.size);
            running.tiles.push(stand_in);
            assert!(running.awaiting.is_empty(), "the deadline cleared the hold");

            let reveal = test_picture(grown.size);
            assert_eq!(
                running.tile_state(wid(1), &reveal),
                TileState::Reveal { fits: true }
            );
            assert_eq!(
                running.tile_state(wid(1), &test_picture(small.size)),
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

        #[test]
        fn every_landed_picture_reaches_the_cache_and_never_downgrades() {
            let mut rng = Gen(93);
            let mut refused = 0usize;
            let mut decisions: Vec<SwapDecision> = Vec::new();
            for _ in 0..RUNS {
                let mut cache: crate::pictures::domain::cache::PictureCache<Picture> =
                crate::pictures::domain::cache::PictureCache::new();
                let size = rng.on_screen().size;
                for _ in 0..rng.below(4) + 1 {
                    let incoming = if rng.coin() {
                        test_picture(size)
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

        /// Once the flight is moving without the picture, a stand-in still counts as a picture waiting
        /// for its reveal, so a settled capture landing early enough replaces it rather than waiting
        /// for the next flight.
        #[test]
        fn a_stand_in_mid_flight_is_swapped_for_its_picture() {
            use crate::pictures::platform::picture::{stand_in_picture, test_bitmap};
            let slot = rect(4.0, 32.0, 859.0, 1081.0);
            let mut flight = flight(Some(Instant::now()));
            let mut tile = stacked(wid(3), slot, slot, Some(2), false);
            tile.snapshot = stand_in_picture(slot.size, test_bitmap());
            flight.tiles.push(tile);

            let state = flight.tile_state(wid(3), &test_picture(slot.size));
            assert_eq!(state, TileState::Reveal { fits: true });
            assert_eq!(
                should_swap_mid_flight(state, true, false, Some(0.3)),
                SwapDecision::Swap("reveal")
            );
        }

        #[test]
        fn a_spawn_entrance_flies_without_a_hold_and_is_a_reveal_in_waiting() {
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
                snapshot: test_picture(spawn.size),
                floating: false,
                server_order: Some(0),
                depth: 0,
                companion: None,
                focused: true,
                radius: None,
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
            let mut running = flight(None);
            running.tiles = vec![tile];
            running.final_frames = vec![(wid(9), slot)];
            running.frames_applied = holding;
            running.duration = Duration::from_millis(350);
            running.apply_at = 0.0;
            running.awaiting = awaiting.clone();
            running.hold_deadline = holding.then(|| Instant::now() + HOLD_CAP);
            running.focus = Some(wid(9));
            assert!(running.awaiting.is_empty());
            assert!(!chase_set.is_empty());
            assert_eq!(running.hold_deadline, None);
            assert!(running.started.is_none() && running.awaiting.is_empty());
            assert_eq!(
                running.tile_state(wid(9), &test_picture(spawn.size)),
                TileState::Reveal { fits: false },
                "the spawn picture does not cover the slot"
            );
            assert_eq!(
                running.tile_state(wid(9), &test_picture(slot.size)),
                TileState::Reveal { fits: true }
            );
            // The plan calls it an entrance; the tile is a loose resize from spawn to slot.
            let mut plan = ReflowPlan::empty();
            plan.entrances.push((wid(9), from, to));
            assert_eq!(plan.member(wid(9)), Some(Member::Entrance { from, to }));
            let targets =
                crate::animation::domain::motion::plan::animation_targets(&FlightPlan::from(plan));
            assert_eq!(targets.len(), 1);
        }
    }

    /// The focus refresh and the pictures a flight asks for.
    mod refresh {
        use super::*;

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

        /// The refresh never fires at frame zero, and its passes are few.
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
            assert!(!fired.is_empty(), "a moving flight refreshes");
            // A second sweep on the same flight fires nothing: the slots are spent.
            assert!(!(0..=100).any(|i| running.take_refresh(i as f64 / 100.0)));
        }

        /// A flight at frame zero asks for its missing pictures at frame zero; holding or
        /// moving, they are warms for the next flight, which the pictures service holds until the
        /// quiet after the lift.
        #[test]
        fn missing_pictures_serve_the_flight_only_at_frame_zero() {
            let mut running = flight(None);
            assert_eq!(running.missing_pictures(), Priority::FrameZero);
            running.awaiting.push((wid(1), CGSize::new(859.0, 1081.0)));
            assert_eq!(running.missing_pictures(), Priority::Warm, "holding");
            running.started = Some(Instant::now());
            assert_eq!(running.missing_pictures(), Priority::Warm, "moving");
            running.awaiting.clear();
            assert_eq!(running.missing_pictures(), Priority::Warm, "moving");
        }
    }

    /// Which tile and which container are drawn in front: `restack` and `band_plan`, over passes.
    mod depth {
        use rini_ipc::protocol::Direction;

        use super::*;
        use super::wid;
        use crate::animation::domain::motion::plan::*;
        use crate::animation::domain::motion::surface::SurfaceGeometry;
        use crate::animation::domain::motion::z_group::{GROUP_STRIDE, MAX_TILE_DEPTH};
        use crate::transitions::domain::strip_move::tests::staging::*;

        fn column(i: f64) -> CGRect {
            rect(4.0 + i * 863.0, 32.0, 859.0, 1081.0)
        }

        fn shifted(frame: CGRect, dx: f64, dy: f64) -> CGRect {
            CGRect::new(
                CGPoint::new(frame.origin.x + dx, frame.origin.y + dy),
                frame.size,
            )
        }

        fn depth(flight: &RunningAnimation, window: WindowId) -> usize {
            flight.tiles.iter().find(|t| t.window == window).unwrap().depth
        }

        /// Depth is banded from the flight's latest focus for every tile, redundant ones included.
        #[test]
        fn depths_do_not_interleave_groups_across_passes() {
            let (s1, s2, f) = (wid(1), wid(2), wid(3));
            let slot_a = rect(4.0, 32.0, 859.0, 1081.0);
            let slot_b = rect(867.0, 32.0, 859.0, 1081.0);
            let slot_c = rect(1730.0, 32.0, 859.0, 1081.0);
            let zoom = rect(224.0, 95.0, 1280.0, 960.0);

            let pass1 = vec![
                stacked(s1, slot_a, slot_a, Some(1), false),
                stacked(s2, slot_c, slot_b, Some(2), false),
                stacked(f, zoom, zoom, Some(0), true),
            ];
            let mut flight = flight(None);
            for (_, outcome) in flight.merge_pass(pass1, None) {
                assert_eq!(outcome, Admitted::Joined);
            }

            let pass2 = vec![
                stacked(s1, slot_a, slot_a, Some(1), false),
                stacked(s2, slot_c, slot_c, Some(2), false),
                stacked(f, zoom, zoom, Some(0), true),
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

        #[test]
        fn a_later_focus_rebands_every_tile_in_the_flight() {
            let (s1, s2, f) = (wid(1), wid(2), wid(3));
            let slot_a = rect(4.0, 32.0, 859.0, 1081.0);
            let slot_b = rect(867.0, 32.0, 859.0, 1081.0);
            let slot_c = rect(1730.0, 32.0, 859.0, 1081.0);
            let zoom = rect(224.0, 95.0, 1280.0, 960.0);
            let mut flight = flight(None);

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
            let mut flight = flight(None);
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
            let mut flight = flight(None);
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
            let mut flight = flight(None);
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

        #[test]
        fn the_unfocused_group_is_never_in_front_of_the_focused_group() {
            let mut rng = Gen(33);
            for _ in 0..RUNS {
                let count = rng.below(6) as u32 + 1;
                let mut flight = flight(None);
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

        /// The 50/50 pair with Settings over them (`z_group::stack`).
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

        /// `container_z - within` is `-depth` exactly, the lifted
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

        /// The moved window is focused and its container is drawn first, so the column it passes
        /// slides beneath it even where the server has that column in front: crossing, and held on
        /// the strip.
        #[test]
        fn the_moved_window_is_drawn_over_the_column_it_passes() {
            for width in [WIDTH, HALF] {
                let step = width + 3.0;
                let frames = [
                    (wid(1), at(4.0 - step, width)),
                    (wid(2), at(4.0, width)),
                    (wid(3), at(4.0 + step, width)),
                ];
                let windows = surface(&frames, wid(2), Direction::Right);
                let (from_offset, to_offset) = (CGPoint::new(-step, 0.0), CGPoint::new(0.0, 0.0));
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
                assert_ne!(moved, passed, "{width}");
                let order = |key| banding.group_order.iter().position(|k| *k == key).unwrap();
                assert_eq!(order(moved), 0, "{width}: the moved window leads");
                assert!(order(moved) < order(passed), "{width}");
            }
        }

        /// Half-width columns: the second press arrives while the first still flies. W stays where
        /// it is drawn and keeps its container, B is taken out of the strip's piece and crosses
        /// beneath W, A (passed by the first press) and everything else carry on with the pan. W is
        /// still drawn over the column it passes.
        #[test]
        fn a_second_move_mid_flight_swaps_its_pair_and_pans_the_rest() {
            let (zero, a, w, b, c) = (wid(1), wid(2), wid(3), wid(4), wid(5));
            let (current, second, incoming, offset) = two_presses(HALF);
            let (merged, delta) = merge_plans(
                &current,
                &incoming,
                Some(pan_travel(offset, REST)),
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
    }

    /// A move's nudge waits for its flight to move, then plays stretched.
    mod nudge {
        use super::*;
        use crate::animation::domain::motion::easing::{EASE_IN_OUT, MOTION_CURVE};

        /// The reactor's step waits with the move's plain duration until the flight moves, then
        /// plays stretched, and the flight crosses on the ease-in-out. A press while one plays is
        /// skipped, and its flight keeps the motion curve.
        #[test]
        fn a_nudge_waits_for_its_flight_then_plays_stretched_on_the_ease_in_out() {
            let step = CGPoint::new(-576.0, 0.0);
            let plain = Duration::from_millis(350);
            let now = Instant::now();
            let idle = NudgePlaying::default();
            let waiting = nudge_waiting(Some(step), plain, &idle, now);
            assert_eq!(waiting, Some((step, plain)));
            let started = starting_nudge(waiting, &idle, now);
            assert_eq!(started, Some((step, plain.mul_f64(NUDGE_STRETCH))));
            assert_eq!(flight_curve(started.is_some()), EASE_IN_OUT);

            let mut playing = NudgePlaying::default();
            playing.start(now, plain.mul_f64(NUDGE_STRETCH));
            let soon = now + Duration::from_millis(100);
            assert_eq!(nudge_waiting(Some(step), plain, &playing, soon), None);
            assert_eq!(starting_nudge(Some((step, plain)), &playing, soon), None);
            assert_eq!(flight_curve(false), MOTION_CURVE);
            assert_eq!(nudge_waiting(None, plain, &idle, now), None);
        }
    }

    /// The edge bounce: every push against an end shows, re-kicked from where the last is drawn.
    mod bounce {
        use super::*;

        /// A push while a bounce plays is not dropped: it starts a bounce from where the last is
        /// drawn. One after it came home, or with none playing, starts from rest.
        #[test]
        fn every_push_at_an_end_starts_a_bounce_from_where_the_last_is_drawn() {
            let plays = Duration::from_millis(350);
            let wall = CGPoint::new(-72.0, 0.0);
            let began = Instant::now();
            let first = next_bounce(None, began, wall, plays);
            assert_eq!(first, Bounce::from_rest(wall, plays));
            let later = began + Duration::from_millis(60);
            let second = next_bounce(Some((began, first)), later, wall, plays);
            assert_eq!(second.from, first.at(Duration::from_millis(60)));
            assert_ne!(second.from, CGPoint::new(0.0, 0.0), "not over from rest");
            let home = began + plays;
            assert_eq!(
                next_bounce(Some((began, first)), home, wall, plays),
                Bounce::from_rest(wall, plays)
            );
        }
    }

    /// Border windows ride the window they trace: its container, its band, its step.
    mod borders {
        use rini_ipc::protocol::Direction;

        use super::*;
        use super::wid;
        use crate::animation::domain::motion::plan::*;
        use crate::animation::domain::motion::surface::SurfaceGeometry;
        use crate::transitions::domain::strip_move::tests::staging::*;

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

        /// A full-width move steps the whole strip away from where W went: W's still piece, the
        /// strip's piece the column it passed is held in, and every strip window's border with
        /// them. A pinned floating window's border has no travel either, but it stands with its
        /// window in the floating container and is drawn in its band, and the step never carries
        /// that container.
        #[test]
        fn a_floating_windows_border_stays_with_it_and_off_the_nudge() {
            let (a, w, f) = (wid(1), wid(2), wid(7));
            let settings = rect(500.0, 300.0, 700.0, 500.0);
            let (mut windows, step) = staged(
                &[
                    (a, at(4.0 - STEP, WIDTH)),
                    (w, at(4.0, WIDTH)),
                    (wid(3), at(4.0 + STEP, WIDTH)),
                ],
                w,
                Direction::Right,
            );
            let step = step.expect("a full-width move steps");
            assert_eq!(step, CGPoint::new(-576.0, 0.0), "away from where W went");
            windows.push(floating_at(f, settings, true));
            let mut plan = surface_plan(&windows, OFFSET, REST);
            let w_key = plan.group_of(w).unwrap().key;
            assert_eq!(w_key, GroupKey::STILL, "the camera followed it");
            let strip = plan.group_of(wid(3)).unwrap().key;
            assert_eq!(plan.group_of(a).unwrap().key, strip, "A is held on the strip");
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
                    test_picture(hug(real).size),
                )
            };
            let f_border = border(f, settings, wid(70));
            let w_border = border(w, at(4.0, WIDTH), wid(20));
            let a_border = border(a, at(4.0 - STEP, WIDTH), wid(21));
            assert!(f_border.floating, "a floating window's border is floating");
            assert_eq!(f_border.companion, Some(f));
            assert_eq!(f_border.depth, tile(f).depth);
            assert!(!w_border.floating);
            plan.adopt(&f_border);
            plan.adopt(&w_border);
            plan.adopt(&a_border);
            let flight = FlightPlan::from(plan);
            assert_eq!(
                flight.member(wid(70)),
                Some(Member::Floating {
                    from: hug(settings),
                    to: hug(settings)
                }),
                "it stands with F"
            );
            assert_eq!(key_of(&flight, wid(20)), Some(w_key), "W's border rides with W");
            assert_eq!(key_of(&flight, wid(21)), Some(strip), "A's border rides with A");
            for group in flight.groups.iter().filter(|g| !g.members.is_empty()) {
                assert!(carries_out_and_back(group.key, step), "{:?} steps", group.key);
            }
            assert!(!carries_out_and_back(GroupKey::Floating, step), "F stays put");
            tiles.extend([f_border, w_border]);
            let banding = band_plan(&flight, &tiles, Some(f));
            assert!(banding.lifted.contains(&f));
            assert!(banding.lifted.contains(&wid(70)), "drawn in front with F");
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
                test_picture(hug(settings).size),
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

        /// A border whose picture lands between two presses joins the second mid-step. It rides
        /// the moved window's container, placed against where that container's leg has it: where
        /// it is drawn carries the step, which would have split the border from its window by the
        /// step.
        #[test]
        fn a_border_arriving_mid_nudge_rides_with_the_moved_window() {
            let w = wid(3);
            let (current, second, mut incoming, offset) = two_presses(WIDTH);
            let windows = surface(&second, w, Direction::Right);
            let moved = windows.iter().find(|s| s.window == w).unwrap();
            let (from, to) = moved.travel(offset, REST);
            let mut anchor = stacked(w, from, to, Some(0), false);
            anchor.depth = 0;
            let real = at(4.0, WIDTH);
            let frame = rect(0.0, 28.0, WIDTH + 8.0, 1089.0);
            let border = border_tile(&anchor, real, (wid(20), frame), test_picture(frame.size));
            incoming.adopt(&border);

            let along = halfway(&current);
            let key = key_of(&current, w).unwrap();
            let on_screen: HashMap<GroupKey, CGPoint> =
                along.iter().map(|(key, p)| (*key, CGPoint::new(p.x - 576.0, p.y))).collect();
            let presented = Presented {
                drawn: on_screen.clone(),
                along,
            };
            let (merged, delta) = merge_plans(
                &current,
                &incoming,
                Some(pan_travel(offset, REST)),
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
                "where W is drawn, step and all"
            );
            assert!(dest(&merged, wid(20)).unwrap().same_as(border.to));
        }
    }

    /// A strip movement never holds, so a stand-in's real picture is chased where it will show.
    mod stand_in_chase {
        use super::*;
        use super::wid;
        use crate::transitions::domain::strip_move::tests::staging::*;

        /// A strip movement starts at once and never holds, so a stand-in's real picture is chased
        /// instead: for the column a move at the edge brings in from its park, not for one parked
        /// at both ends, and never for a window drawn from its own picture. Where the window lands
        /// is its real frame: a column a full-width move holds on the strip is drawn going beneath
        /// the moved window while its real window parks, so it is not chased.
        #[test]
        fn a_stand_in_coming_on_screen_is_chased_and_one_staying_parked_is_not() {
            use crate::pictures::platform::picture::{stand_in_picture, test_bitmap};
            let slot = rect(868.0, 32.0, 860.0, 1081.0);
            let park = rect(1732.0, 32.0, 860.0, 1081.0);
            let stand_in = |window, from: CGRect, to: CGRect| {
                let mut tile = stacked(window, from, to, Some(1), false);
                tile.snapshot = stand_in_picture(to.size, test_bitmap());
                tile
            };
            let held = at(4.0, WIDTH);
            let tiles = [
                stand_in(wid(1), park, slot),
                stand_in(wid(2), at(4.0 - 3.0 * STEP, WIDTH), at(4.0 - 2.0 * STEP, WIDTH)),
                stacked(wid(3), park, slot, Some(2), false),
                stand_in(wid(4), at(4.0 + STEP, WIDTH), held),
            ];
            let final_frames = [(wid(1), slot), (wid(4), at(4.0 - 2.0 * STEP, WIDTH))];
            assert_eq!(
                stand_in_chase(&tiles, &final_frames, DISPLAY),
                vec![(wid(1), slot.size)]
            );
            assert_eq!(
                stand_in_chase(&tiles, &[], DISPLAY),
                vec![(wid(1), slot.size), (wid(4), held.size)],
                "with no frame to apply, a window lands where its tile does"
            );
        }
    }
}
