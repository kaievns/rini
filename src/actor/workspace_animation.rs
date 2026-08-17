//! Drives the capture-based animation overlay.
//!
//! Owns the overlay and the snapshot cache, and runs on the main thread because Core Animation
//! requires it. See `crate::ui::workspace_overlay` for why the animation draws pictures instead of
//! moving windows, and `docs/capture-overlay-research.md` for the measurements behind it.
//!
//! The shape of one animation:
//!
//! 1. Capture every participating window. Fresh from SkyLight when the window is fully on screen,
//!    from the cache otherwise, because the only API that can capture an off-strip window costs
//!    about 40ms plus 14.5ms per window and cannot run at switch time.
//! 2. Build a tile per window with its start and end frames, show the overlay, and let the caller
//!    place the real windows at their final frames underneath. They are covered, so the fact that
//!    each app answers Accessibility at its own speed stops being visible.
//! 3. Step the tiles to the target over the animation duration, one Core Animation transaction per
//!    frame, so the windows cannot tear against each other.
//! 4. Hide the overlay, revealing real windows already in place.
//!
//! The frame clock is time-based rather than a frame counter. A counter assumes every frame is
//! delivered on time, and when one is late the animation silently slows down instead of skipping,
//! which is what made the old engine feel inconsistent.

use std::time::{Duration, Instant};

use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_foundation::MainThreadMarker;
use tracing::{debug, warn};

use crate::actor;
use crate::actor::app::WindowId;
use crate::sys::run_loop::RepeatingTimer;
use crate::sys::window_server::WindowServerId;
use crate::ui::snapshot_service::{SnapshotService, SnapshotTarget};
use crate::ui::window_snapshot::{SnapshotCache, WindowSnapshot, capture_via_skylight};
use crate::ui::workspace_overlay::{CanvasTile, OverlayTile, WorkspaceOverlay};

/// One window's fixed place on the canvas.
///
/// The canvas holds every window across every workspace involved in a movement, laid out as one
/// continuous surface: x is the strip position, y is the workspace stacked below the one above it.
#[derive(Debug, Clone)]
pub struct CanvasWindow {
    pub window: WindowId,
    pub server_id: WindowServerId,
    /// Position on the canvas, never interpolated.
    pub frame: CGRect,
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
}

#[derive(Debug)]
pub enum Event {
    /// Animate a set of windows. The caller must have already placed the real windows at their
    /// final frames, or arrange to do so immediately after sending this.
    Animate { windows: Vec<AnimationRequest>, duration: Duration },
    /// Display geometry for the overlay. Must be the USABLE frame, excluding the menu bar strip,
    /// so the user's bar is not covered and made to flicker.
    SetDisplay { frame: CGRect, scale: f64 },
    /// Refresh the cached snapshot of one window, for windows that are off-strip and so cannot be
    /// captured usefully at switch time.
    RefreshSnapshot { window: WindowId, server_id: WindowServerId, size: CGSize },
    /// Drop snapshots for windows that no longer exist, so the cache cannot grow without bound.
    ForgetWindow(WindowId),
    /// Slide every currently visible window in from an offset, purely to evaluate animation quality
    /// by eye. Does not touch any real window, so it is safe to fire at any time.
    DebugSlide { dx: f64, dy: f64, duration: Duration },
    /// Move the viewport across a canvas holding every window involved, rather than moving each window
    /// separately.
    ///
    /// This is the shape a scrolling strip and a stack of workspaces actually have. Animating windows
    /// individually could not convey distance: a jump from workspace 1 to 4, or across the whole
    /// strip, looked exactly like a step to the neighbour because everything in between was off screen
    /// at both ends and so never drawn. Moving one canvas scrolls all of it past.
    AnimateCanvas {
        windows: Vec<CanvasWindow>,
        from_offset: CGPoint,
        to_offset: CGPoint,
        /// Real screen frames to apply once the overlay is covering them.
        final_frames: Vec<(WindowId, CGRect)>,
        duration: Duration,
    },
    /// One frame of the running animation. Posted by the run loop timer, not by any other actor.
    Tick,
    /// The layout passes have settled; start the clock. Posted by the coalesce timer.
    StartMoving,
    /// A background capture has landed. Posted by the snapshot service, not by another actor.
    SnapshotsReady,
    /// Capture every managed window that SkyLight cannot serve, so the cache is warm before the next
    /// animation. Safe to call at any time; it only queues background work.
    ///
    /// Targets come from the reactor because only it knows each window's real [`WindowId`]. Deriving
    /// ids from the window server instead produced keys that never matched the ones an animation
    /// looks up, so the cache filled and was never read.
    WarmWindows(Vec<SnapshotTarget>),
    /// Warm from the window server rather than from rini's own window table. Only for the debug
    /// command, where there is no reactor-supplied window set.
    WarmCache,
}

pub type Sender = actor::Sender<Event>;
pub type Receiver = actor::Receiver<Event>;

/// Frame interval. 60fps rather than 120 because the previous engine was configured at 120 and the
/// per-frame work could not keep up, so frames were dropped and the result was less smooth than a
/// slower rate that always lands. Nothing here writes to another process, so this may be worth
/// revisiting once the overlay is doing the drawing.
const FRAME_INTERVAL: Duration = Duration::from_micros(16_667);

/// How long to keep collecting windows before the animation starts moving.
///
/// The reactor arranges a layout over SEVERAL passes, and each pass reports only the windows it
/// resolved. Measured on one workspace switch: three passes of a single window, then a fourth of
/// four. Treating each pass as its own animation restarted the motion four times.
///
/// So the overlay goes up immediately, showing every window at the position it is leaving, and the
/// clock only starts once the passes settle. Because nothing has moved yet, windows joining during
/// this window cannot pop. One frame is enough to absorb the passes and is imperceptible.
const COALESCE_WINDOW: Duration = Duration::from_millis(25);

/// How far through the animation the real windows are placed.
///
/// Late enough that the overlay is certainly covering them, and early enough that the Accessibility
/// writes have time to land before it comes down. Each write is a synchronous request into another
/// process, and the slowest apps measured around 24ms, so this leaves roughly a fifth of the
/// animation for them to finish.
const APPLY_FRAMES_AT: f64 = 0.75;

/// A canvas movement in flight: the tiles never move, the viewport does.
struct RunningCanvas {
    from_offset: CGPoint,
    to_offset: CGPoint,
    final_frames: Vec<(WindowId, CGRect)>,
    frames_applied: bool,
    started: Instant,
    duration: Duration,
    _clock: Option<RepeatingTimer>,
}

impl RunningCanvas {
    fn progress(&self) -> f64 {
        if self.duration.is_zero() {
            return 1.0;
        }
        (self.started.elapsed().as_secs_f64() / self.duration.as_secs_f64()).clamp(0.0, 1.0)
    }

    fn offset_at(&self, eased: f64) -> CGPoint {
        CGPoint::new(
            self.from_offset.x + (self.to_offset.x - self.from_offset.x) * eased,
            self.from_offset.y + (self.to_offset.y - self.from_offset.y) * eased,
        )
    }
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
    /// Dropped when the animation ends, which invalidates the timer and stops the wakeups.
    _clock: Option<RepeatingTimer>,
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

    /// Adds or retargets one window without disturbing anything already moving.
    fn merge(&mut self, tile: OverlayTile) {
        if let Some(existing) = self.tiles.iter_mut().find(|t| t.window == tile.window) {
            // Keep the original start so a window already moving is not yanked backwards, and take
            // the newer destination so the animation ends where the window really goes.
            existing.to = tile.to;
            existing.snapshot = tile.snapshot;
            existing.depth = tile.depth;
        } else {
            self.tiles.push(tile);
        }
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
    running: Option<RunningAnimation>,
    /// A canvas movement in flight, which supersedes the per-window path while it runs.
    canvas: Option<RunningCanvas>,
    /// Fires once after the layout passes settle, to start the animation moving.
    coalesce: Option<RepeatingTimer>,
    /// Windows from the most recent animation, so the post-animation refresh uses real ids.
    last_animated: Vec<SnapshotTarget>,
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
            running: None,
            canvas: None,
            coalesce: None,
            last_animated: Vec::new(),
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
            Event::SetDisplay { frame, scale } => self.set_display(frame, scale),
            Event::Animate { windows, duration } => self.start(windows, duration),
            Event::AnimateCanvas {
                windows,
                from_offset,
                to_offset,
                final_frames,
                duration,
            } => self.start_canvas(windows, from_offset, to_offset, final_frames, duration),
            Event::RefreshSnapshot { window, server_id, size } => {
                self.refresh_snapshot(window, server_id, size)
            }
            Event::ForgetWindow(window) => self.cache.forget(window),
            Event::DebugSlide { dx, dy, duration } => self.debug_slide(dx, dy, duration),
            Event::Tick => {
                // A canvas movement owns the frame clock while it runs.
                if self.canvas.is_some() {
                    self.step_canvas();
                } else {
                    self.step();
                }
            }
            Event::StartMoving => self.start_moving(),
            Event::SnapshotsReady => self.collect_snapshots(),
            Event::WarmCache => self.warm_cache(),
            Event::WarmWindows(targets) => self.warm_windows(targets),
        }
    }

    /// Moves completed background captures into the cache.
    ///
    /// `SnapshotCache::insert` refuses to replace a usable capture with a clipped one, so a result
    /// that lands late cannot downgrade what is already held.
    fn collect_snapshots(&mut self) {
        let landed = self.service.collect();
        if landed.is_empty() {
            return;
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
            self.cache.insert(window, snapshot);
        }
    }

    /// Queues background captures for a set of windows the reactor identified.
    ///
    /// Already-held windows are skipped by the service, and the cache keeps what it has unless
    /// something better arrives, so calling this after every switch settles rather than re-capturing.
    fn warm_windows(&mut self, targets: Vec<SnapshotTarget>) {
        let wanted: Vec<SnapshotTarget> = targets
            .into_iter()
            // Anything already drawable needs nothing. Everything else is worth a real capture.
            .filter(|target| self.cache.usable(target.window).is_none())
            .collect();
        if wanted.is_empty() {
            return;
        }
        debug!(count = wanted.len(), "warming snapshots for reactor-supplied windows");
        self.service.request(wanted);
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

    fn set_display(&mut self, frame: CGRect, scale: f64) {
        let first = self.display.is_none();
        let changed = self.display != Some((frame, scale));
        self.display = Some((frame, scale));
        self.service.set_scale(scale);
        if let Some(overlay) = self.overlay.as_mut() {
            overlay.set_frame(frame, scale);
        }
        // Warm on the first geometry, and after any change, so the very first switch has pixels
        // rather than being the one that fills the cache for later switches.
        if first || changed {
            self.warm_cache();
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

    fn refresh_snapshot(&mut self, window: WindowId, server_id: WindowServerId, size: CGSize) {
        let scale = self.display.map(|(_, scale)| scale).unwrap_or(2.0);
        if let Some(snapshot) = capture_via_skylight(server_id, (size.width, size.height), scale) {
            self.cache.insert(window, snapshot);
        }
    }

    /// The snapshot to draw for one window, from the cache only.
    ///
    /// Deliberately captures NOTHING here. Capturing at switch time was the single worst thing about
    /// the first version: a SkyLight capture costs 12ms to 24ms and runs on the main thread, so an
    /// 18 window workspace spent roughly 400ms capturing before the animation could start. That is
    /// the lag between pressing the key and seeing anything move.
    ///
    /// A window with nothing cached simply does not animate. It is placed at its destination when the
    /// overlay comes down, and a background capture is queued so the next switch has it.
    fn snapshot_for(&mut self, request: &AnimationRequest) -> Option<WindowSnapshot> {
        self.cache.usable(request.window).cloned()
    }

    /// Does this window appear on screen at ANY point during the animation?
    ///
    /// Testing only the endpoints was wrong in a way that mattered. On a scrolling strip, jumping
    /// between distant columns moves intermediate windows straight across the display, and those are
    /// exactly the windows that show how far the strip travelled. Excluding them made a jump across
    /// the whole strip look identical to a jump between neighbours, with the new column simply
    /// appearing from one side and no sense of distance at all.
    ///
    /// So the whole path is sampled, not just its ends. A window parked off-strip that never crosses
    /// the display is still excluded, which is what stopped the slivers flickering in along both
    /// edges, but one that sweeps through is now drawn.
    fn is_worth_animating(&self, from: CGRect, to: CGRect, display: CGRect) -> bool {
        /// Fraction of the window that must be on screen at some sampled moment.
        ///
        /// Lower than an endpoint test would need, because a window crossing the display is only
        /// partly on it for most of the crossing, and clipping it out would leave a gap in the very
        /// motion that conveys distance.
        const MIN_ON_SCREEN: f64 = 0.25;
        /// Samples along the path. Enough that a window cannot cross the display between two of them:
        /// the fastest realistic travel is a few display widths, so eleven samples leave any crossing
        /// window on screen for at least one of them.
        const SAMPLES: usize = 11;

        let area = from.size.width * from.size.height;
        if area <= 0.0 {
            return false;
        }
        (0..SAMPLES).any(|step| {
            let t = step as f64 / (SAMPLES - 1) as f64;
            let at = crate::ui::workspace_overlay::lerp_rect(from, to, t);
            on_screen_fraction(at, display) >= MIN_ON_SCREEN
        })
    }

    fn start(&mut self, windows: Vec<AnimationRequest>, duration: Duration) {
        if windows.is_empty() {
            return;
        }
        let Some((display_frame, _)) = self.display else {
            debug!("no display geometry yet; skipping animation");
            return;
        };

        // Every window's destination, whether or not it has a picture. A window with no snapshot is
        // not drawn, but it still has to be placed.
        let final_frames: Vec<(WindowId, CGRect)> =
            windows.iter().map(|request| (request.window, request.to)).collect();

        // Front-to-back order straight from the window server, so the overlay stacks tiles the way
        // the screen is actually stacked.
        let depths = crate::sys::window_server::front_to_back_depths();

        let mut tiles = Vec::with_capacity(windows.len());
        let mut skipped = 0usize;
        let mut offscreen = 0usize;
        let mut needs_capture: Vec<SnapshotTarget> = Vec::new();
        for request in &windows {
            let start = actual_start(request);
            // Parked slivers are excluded on the way in AND on the way out: a window arriving from
            // off-strip has no visible starting point, and one leaving has no visible destination.
            if !self.is_worth_animating(start, request.to, display_frame) {
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
                Some(snapshot) => tiles.push(OverlayTile {
                    window: request.window,
                    from: to_overlay_space(start, display_frame),
                    to: to_overlay_space(request.to, display_frame),
                    snapshot,
                    // Unknown windows sort behind everything known, which is the safe default: a tile
                    // drawn too far back is far less noticeable than one drawn over the front window.
                    depth: depths
                        .get(&request.server_id.as_u32())
                        .copied()
                        .unwrap_or(usize::MAX / 2),
                }),
                // No usable picture. Better to leave the window out than to draw a smear: it will
                // simply appear at its destination when the overlay drops.
                None => skipped += 1,
            }
        }
        if !needs_capture.is_empty() {
            debug!(
                count = needs_capture.len(),
                "queueing background captures for windows SkyLight could not serve"
            );
            self.service.request(needs_capture);
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
        // nothing was drawable, both use keys an animation will actually look up.
        self.last_animated = windows
            .iter()
            .map(|request| SnapshotTarget {
                window: request.window,
                server_id: request.server_id,
                size: request.to.size,
            })
            .collect();

        if tiles.is_empty() {
            // Nothing drawable, so there is no overlay to hide behind: place the windows at once
            // rather than leaving them where they are.
            self.request_frames(final_frames);
            // Warm anyway, or this deadlocks: the cache only ever filled when an animation completed,
            // and no animation could run with an empty cache.
            let targets = std::mem::take(&mut self.last_animated);
            self.warm_windows(targets);
            return;
        }

        // Merge into the animation already in flight rather than replacing it. Two reasons.
        //
        // The reactor arranges a layout over several passes, so treating each as its own animation
        // restarted the motion four times on a measured switch.
        //
        // And a later pass can CHANGE where a window is going, which is why retargeting is allowed
        // even after the motion has started. Ignoring those updates left the animation ending
        // somewhere the windows did not, so the real windows visibly shifted at the handover, by a
        // little for the focused window and a lot for its neighbours.
        if let Some(running) = self.running.as_mut() {
            {
                for (window, frame) in final_frames {
                    if let Some(existing) =
                        running.final_frames.iter_mut().find(|(w, _)| *w == window)
                    {
                        existing.1 = frame;
                    } else {
                        running.final_frames.push((window, frame));
                    }
                }
                for tile in tiles {
                    running.merge(tile);
                }
                // A window joining or retargeting mid-flight is drawn at the CURRENT progress, so it
                // lands with everything else rather than snapping when the overlay lifts.
                let progress = running.progress();
                let tiles = std::mem::take(&mut running.tiles);
                if let Some(overlay) = self.overlay.as_mut() {
                    overlay.set_tiles(&tiles);
                    overlay.draw_frame(&tiles, progress);
                }
                if let Some(running) = self.running.as_mut() {
                    running.tiles = tiles;
                    // The destinations changed, so the frames already requested are stale. Ask again
                    // once the animation is far enough along.
                    running.frames_applied = false;
                }
                return;
            }
        }

        let Some(overlay) = self.ensure_overlay() else { return };
        overlay.set_tiles(&tiles);
        overlay.draw_frame(&tiles, 0.0);
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

        self.running = Some(RunningAnimation {
            tiles,
            final_frames,
            frames_applied: false,
            started: None,
            duration,
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

        // Start moving once the layout passes have settled.
        let tx = self.tx.clone();
        self.coalesce = RepeatingTimer::every(COALESCE_WINDOW, move || {
            _ = tx.send(Event::StartMoving);
        });
    }

    /// Animates the viewport across a canvas of every window involved.
    fn start_canvas(
        &mut self,
        windows: Vec<CanvasWindow>,
        from_offset: CGPoint,
        to_offset: CGPoint,
        final_frames: Vec<(WindowId, CGRect)>,
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
        for window in &windows {
            match self.cache.usable(window.window).cloned() {
                Some(snapshot) => tiles.push(CanvasTile {
                    window: window.window,
                    frame: window.frame,
                    snapshot,
                    depth: depths
                        .get(&window.server_id.as_u32())
                        .copied()
                        .unwrap_or(usize::MAX / 2),
                }),
                None => missing += 1,
            }
        }
        debug!(
            requested = windows.len(),
            tiles = tiles.len(),
            missing,
            travel = format!(
                "{:.0},{:.0} -> {:.0},{:.0}",
                from_offset.x, from_offset.y, to_offset.x, to_offset.y
            ),
            "canvas animation"
        );

        if tiles.is_empty() {
            // Nothing to draw. If a canvas is already running, leave it alone: cancelling a good
            // animation to show nothing is worse than ignoring this request. Placing the real frames
            // now would also yank them out from behind the running overlay.
            if self.canvas.is_some() {
                let targets = std::mem::take(&mut self.last_animated);
                self.warm_windows(targets);
                return;
            }
            // Otherwise there is no cover, so place the windows rather than leaving them adrift.
            self.request_frames(final_frames);
            let targets = std::mem::take(&mut self.last_animated);
            self.warm_windows(targets);
            return;
        }

        // Carry over whatever the running animation had left to travel, so a rapid sequence of
        // presses reads as one continuous scroll instead of a series of restarts.
        let from_offset = match self.canvas.as_ref() {
            Some(running) => {
                let eased = crate::ui::workspace_overlay::ease_out_cubic(running.progress());
                let current = running.offset_at(eased);
                let residual = CGPoint::new(
                    current.x - running.to_offset.x,
                    current.y - running.to_offset.y,
                );
                debug!(
                    residual = format!("{:.0},{:.0}", residual.x, residual.y),
                    "chaining onto an animation already in flight"
                );
                CGPoint::new(from_offset.x + residual.x, from_offset.y + residual.y)
            }
            None => from_offset,
        };

        // Refreshed per animation: the desktop can change, and it is one cheap framebuffer capture of
        // fully visible windows, which is the case SkyLight handles well.
        let backdrop = self.capture_backdrop();
        let foreground = self.capture_bar();

        let Some(overlay) = self.ensure_overlay() else {
            self.request_frames(final_frames);
            return;
        };
        overlay.set_backdrop(backdrop.as_ref());
        overlay.set_foreground(foreground.as_ref());
        overlay.set_canvas(&tiles);
        overlay.set_canvas_offset(from_offset);
        overlay.show();

        let tx = self.tx.clone();
        let clock = RepeatingTimer::every(FRAME_INTERVAL, move || {
            _ = tx.send(Event::Tick);
        });

        // Any per-window animation is abandoned: the canvas covers the same ground.
        self.running = None;
        self.coalesce = None;
        self.canvas = Some(RunningCanvas {
            from_offset,
            to_offset,
            final_frames,
            frames_applied: false,
            started: Instant::now(),
            duration,
            _clock: clock,
        });

        // With no clock nothing would advance, so land it immediately.
        if self.canvas.as_ref().is_some_and(|c| c._clock.is_none()) {
            self.step_canvas_to_end();
        }
    }

    fn step_canvas(&mut self) {
        let (done, place_now) = {
            let Self { overlay, canvas, .. } = self;
            let Some(canvas) = canvas.as_mut() else { return };
            let progress = canvas.progress();
            let eased = crate::ui::workspace_overlay::ease_out_cubic(progress);
            if let Some(overlay) = overlay.as_mut() {
                overlay.set_canvas_offset(canvas.offset_at(eased));
            }
            let place_now = !canvas.frames_applied && progress >= APPLY_FRAMES_AT;
            if place_now {
                canvas.frames_applied = true;
            }
            (progress >= 1.0, place_now)
        };
        if place_now {
            let frames =
                self.canvas.as_ref().map(|c| c.final_frames.clone()).unwrap_or_default();
            self.request_frames(frames);
        }
        if done {
            self.finish_canvas();
        }
    }

    fn step_canvas_to_end(&mut self) {
        if let (Some(overlay), Some(canvas)) = (self.overlay.as_mut(), self.canvas.as_ref()) {
            overlay.set_canvas_offset(canvas.to_offset);
        }
        let frames = self.canvas.as_ref().map(|c| c.final_frames.clone()).unwrap_or_default();
        self.request_frames(frames);
        self.finish_canvas();
    }

    fn finish_canvas(&mut self) {
        if let Some(overlay) = self.overlay.as_mut() {
            overlay.hide();
            overlay.release_tiles();
        }
        self.canvas = None;
        let targets = std::mem::take(&mut self.last_animated);
        if !targets.is_empty() {
            self.warm_windows(targets);
        }
    }

    /// Starts the clock on an animation that is on screen but not yet moving.
    fn start_moving(&mut self) {
        // Dropping the timer stops it repeating; it only ever needed to fire once.
        self.coalesce = None;
        let Some(running) = self.running.as_mut() else { return };
        if running.started.is_some() {
            return;
        }
        debug!(windows = running.tiles.len(), "starting the animation after coalescing");
        running.started = Some(Instant::now());
    }

    fn step(&mut self) {
        let (done, place_now) = {
            let Self { overlay, running, .. } = self;
            let Some(running) = running.as_mut() else { return };
            let progress = running.progress();
            if let Some(overlay) = overlay.as_mut() {
                overlay.draw_frame(&running.tiles, progress);
            }
            let place_now = !running.frames_applied && progress >= APPLY_FRAMES_AT;
            if place_now {
                running.frames_applied = true;
            }
            (running.is_done(), place_now)
        };
        if place_now {
            let frames = self
                .running
                .as_ref()
                .map(|running| running.final_frames.clone())
                .unwrap_or_default();
            self.request_frames(frames);
        }
        if done {
            self.finish();
        }
    }

    /// Logs how far each real window is from where its tile finished.
    ///
    /// This is the handover shift, measured rather than eyeballed: a non-zero delta here is exactly
    /// the jump seen when the overlay lifts. Kept because it is the only way to tell a layout that
    /// moved from an animation that ended in the wrong place.
    fn report_handover_error(&self) {
        let Some(running) = self.running.as_ref() else { return };
        let Some((display_frame, _)) = self.display else { return };
        let mut worst = 0.0f64;
        let mut worst_wsid = 0u32;
        for (window, intended) in &running.final_frames {
            let Some(target) = running.tiles.iter().find(|t| t.window == *window) else {
                continue;
            };
            let _ = target;
            let Some(info) = crate::sys::window_server::get_window(
                crate::sys::window_server::WindowServerId::new(window.idx.get()),
            ) else {
                continue;
            };
            let dx = (info.frame.origin.x - intended.origin.x).abs();
            let dy = (info.frame.origin.y - intended.origin.y).abs();
            let error = dx.max(dy);
            if error > worst {
                worst = error;
                worst_wsid = window.idx.get();
            }
        }
        let _ = display_frame;
        if worst > 2.0 {
            debug!(
                worst_pt = format!("{:.0}", worst),
                wsid = worst_wsid,
                "handover mismatch: a real window is not where its tile finished"
            );
        }
    }

    /// Captures the desktop wallpaper and icons as one image, for the overlay's backdrop.
    fn capture_backdrop(&self) -> Option<WindowSnapshot> {
        let (display_frame, scale) = self.display?;
        let windows = crate::sys::window_server::desktop_backdrop_windows(display_frame);
        crate::ui::window_snapshot::capture_composite_via_skylight(
            &windows,
            (display_frame.size.width, display_frame.size.height),
            scale,
        )
    }

    /// Captures the bar, so the overlay can redraw it on top of itself.
    fn capture_bar(&self) -> Option<WindowSnapshot> {
        let (display_frame, scale) = self.display?;
        let windows = crate::sys::window_server::bar_windows(display_frame);
        crate::ui::window_snapshot::capture_composite_via_skylight(
            &windows,
            (display_frame.size.width, display_frame.size.height),
            scale,
        )
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
                overlay.draw_frame(&running.tiles, 1.0);
            }
        }
        self.finish();
    }

    fn finish(&mut self) {
        self.report_handover_error();
        if let Some(overlay) = self.overlay.as_mut() {
            overlay.hide();
            // Free the tile contents rather than hold window pictures that are no longer drawn.
            overlay.release_tiles();
        }
        // Dropping the animation drops its timer, which stops the wakeups.
        self.running = None;
        self.coalesce = None;

        // Capture what just became visible, so switching back has pixels ready. Event-driven, once
        // per animation, never on a timer.
        let targets = std::mem::take(&mut self.last_animated);
        if !targets.is_empty() {
            self.warm_windows(targets);
        }
    }

    /// Slides every window currently on screen in from an offset. For judging animation quality by
    /// eye without touching a single real window, so it can be run at any time without risk.
    fn debug_slide(&mut self, dx: f64, dy: f64, duration: Duration) {
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
                from: CGRect::new(
                    CGPoint::new(frame.origin.x + dx, frame.origin.y + dy),
                    frame.size,
                ),
                to: frame,
            })
            .collect();
        debug!(count = requests.len(), dx, dy, "running debug slide");
        self.start(requests, duration);
    }
}

/// What fraction of `frame`'s area lies inside `display`.
fn on_screen_fraction(frame: CGRect, display: CGRect) -> f64 {
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
fn actual_start(request: &AnimationRequest) -> CGRect {
    match crate::sys::window_server::get_window(request.server_id) {
        Some(info) if info.frame.size.width > 0.0 && info.frame.size.height > 0.0 => info.frame,
        _ => request.from,
    }
}

/// A stable [`WindowId`] derived from a window server id.
///
/// The debug paths work from the window server rather than from rini's own window table, so they need
/// a key that is consistent between capturing and drawing. Using pid 0 keeps these clear of real
/// window ids, which always carry a real pid.
fn synthetic_window_id(server_id: WindowServerId) -> WindowId {
    WindowId { pid: 0, idx: std::num::NonZeroU32::new(server_id.as_u32().max(1)).unwrap() }
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

    fn rect(x: f64, y: f64, w: f64, h: f64) -> CGRect {
        CGRect::new(CGPoint::new(x, y), CGSize::new(w, h))
    }

    #[test]
    fn overlay_space_subtracts_the_overlay_origin() {
        // The real case: a display frame inset by a 32pt menu bar. A window at y = 32 must land at
        // y = 0 inside the overlay, or the entire animation is drawn 32pt too low.
        let overlay = rect(0.0, 32.0, 1728.0, 1085.0);
        let window = rect(865.0, 32.0, 859.0, 1081.0);
        assert_eq!(to_overlay_space(window, overlay), rect(865.0, 0.0, 859.0, 1081.0));
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
            _clock: None,
        };
        assert!(running.progress() < 0.2, "just started");

        let finished = RunningAnimation {
            tiles: Vec::new(),
            final_frames: Vec::new(),
            frames_applied: false,
            started: Some(Instant::now() - Duration::from_secs(5)),
            duration: Duration::from_millis(180),
            _clock: None,
        };
        // Clamped rather than allowed past 1.0, since the easing would otherwise overshoot the
        // target position when a frame arrives late.
        assert_eq!(finished.progress(), 1.0);
        assert!(finished.is_done());
    }
}
