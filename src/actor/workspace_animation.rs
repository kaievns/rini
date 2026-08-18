//! Drives the capture-based animation overlay.
//!
//! Owns the overlay and the snapshot cache, and runs on the main thread because Core Animation
//! requires it.
//!
//! One animation: capture the participating windows, build a tile each, show the overlay, let the
//! caller place the real windows underneath while they are covered, step the tiles over the duration,
//! then hide the overlay. The frame clock is time-based rather than a frame counter, so a late frame
//! skips instead of slowing the animation down.
//!
//! Measurements behind all of this are in `docs/capture-overlay-research.md`.

use std::collections::HashMap;
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
    SetDisplay { id: u32, frame: CGRect, scale: f64 },
    /// Refresh the cached snapshot of one window, for windows that are off-strip and so cannot be
    /// captured usefully at switch time.
    RefreshSnapshot { window: WindowId, server_id: WindowServerId, size: CGSize },
    /// Drop snapshots for windows that no longer exist, so the cache cannot grow without bound.
    ForgetWindow(WindowId),
    /// Slide every currently visible window in from an offset, purely to evaluate animation quality
    /// by eye. Does not touch any real window, so it is safe to fire at any time.
    DebugSlide { dx: f64, dy: f64, duration: Duration },
    /// Move the viewport across a canvas holding every window involved, rather than moving each window
    /// separately, so a long jump scrolls past everything in between instead of cutting to the
    /// destination.
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
    /// A mid-flight recapture of the window being switched into has landed. Posted by the capture
    /// thread, not by another actor.
    PictureReady { window: WindowId, snapshot: WindowSnapshot },
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

/// Frame interval. 60fps rather than 120 because the previous engine was configured at 120 and the
/// per-frame work could not keep up, so frames were dropped and the result was less smooth than a
/// slower rate that always lands. Nothing here writes to another process, so this may be worth
/// revisiting once the overlay is doing the drawing.
const FRAME_INTERVAL: Duration = Duration::from_micros(16_667);

/// How long to keep collecting windows before the animation starts moving.
///
/// The reactor arranges a layout over several passes, and treating each as its own animation restarted
/// the motion. The overlay goes up immediately and the clock starts once the passes settle, so windows
/// joining in between cannot pop. One frame is enough and is imperceptible.
const COALESCE_WINDOW: Duration = Duration::from_millis(25);

/// How many windows may be captured fresh at switch time.
///
/// Capturing everything cost roughly 400ms before anything moved. SkyLight can only capture what is on
/// screen anyway, and that is the set whose staleness is visible, so this bounds the cost at about a
/// frame.
const MAX_FRESH_CAPTURES: usize = 3;

/// How far into a movement to recapture the window being switched into.
///
/// Early, so the corrected picture is on screen for most of the flight, but not on the very first frame,
/// because the destination workspace's windows are only shown once the reactor has acted on the switch.
const REFRESH_DESTINATION_AT: f64 = 0.12;

/// How many windows to recapture mid-flight. One: this exists for the window being switched into, and
/// each capture costs a frame.
const MAX_DESTINATION_CAPTURES: usize = 1;

/// Which of `candidates` to recapture, frontmost first, capped at `max`.
///
/// Frontmost first because the front window is the one being switched INTO, and the one whose picture
/// matters most. Capped because each capture costs a frame.
fn refresh_order(
    candidates: &[(WindowId, u32)],
    depths: &HashMap<u32, usize>,
    on_screen: &[u32],
    max: usize,
) -> Vec<WindowId> {
    let mut ordered: Vec<&(WindowId, u32)> =
        candidates.iter().filter(|(_, wsid)| on_screen.contains(wsid)).collect();
    ordered.sort_by_key(|(_, wsid)| depths.get(wsid).copied().unwrap_or(usize::MAX));
    ordered.into_iter().take(max).map(|(window, _)| *window).collect()
}

/// How much of a window must be on screen before a fresh capture is worth attempting. A partly covered
/// window comes back clipped and would be rejected anyway.
const FRESH_CAPTURE_MIN_ON_SCREEN: f64 = 0.99;

/// How far through the animation the real windows are placed. Late enough that the overlay is
/// certainly covering them, early enough that the Accessibility writes land before it lifts.
const APPLY_FRAMES_AT: f64 = 0.75;

/// A canvas movement in flight: the tiles never move, the viewport does.
struct RunningCanvas {
    from_offset: CGPoint,
    to_offset: CGPoint,
    final_frames: Vec<(WindowId, CGRect)>,
    frames_applied: bool,
    started: Instant,
    duration: Duration,
    /// Frames actually drawn. Reported when the movement ends, because the difference between a smooth
    /// animation and one the eye reads as an instant cut is entirely how many frames reached the screen,
    /// and that cannot be recovered from a screen recording: macOS records at a variable rate and emits
    /// nothing while the screen is unchanged.
    frames: u32,
    /// The windows on the canvas, with their server ids and drawn sizes, so the one being switched into
    /// can be found again mid-flight.
    tiles: Vec<(WindowId, WindowServerId, CGSize)>,
    /// Whether the window being switched INTO has been recaptured since it came on screen.
    ///
    /// It cannot be recaptured when the movement starts, because the destination workspace's windows are
    /// not on screen yet, so its picture is whatever it last had. If that was taken while the app was
    /// unfocused, the tile slides in dimmed and snaps to focused at the handover.
    destination_refreshed: bool,
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
    /// Which display the overlay is on, so the desktop can be captured for that screen. Kept beside
    /// `display` rather than folded into it because only the desktop capture needs it.
    display_id: Option<u32>,
    running: Option<RunningAnimation>,
    /// A canvas movement in flight, which supersedes the per-window path while it runs.
    canvas: Option<RunningCanvas>,
    /// Fires once after the layout passes settle, to start the animation moving.
    coalesce: Option<RepeatingTimer>,
    /// Windows from the most recent animation, so the post-animation refresh uses real ids.
    last_animated: Vec<SnapshotTarget>,
    /// Whether a usable desktop has ever been drawn behind the strips. Until one has, even a capture
    /// missing its wallpaper is worth drawing, because the alternative is the bare black window.
    has_backdrop: bool,
    /// The desktop as ScreenCaptureKit rendered it, which is the only source that reliably includes
    /// the wallpaper. Held here rather than re-requested per animation because it costs about 40ms.
    desktop: Option<WindowSnapshot>,
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
            canvas: None,
            coalesce: None,
            last_animated: Vec::new(),
            has_backdrop: false,
            desktop: None,
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
            Event::PictureReady { window, snapshot } => self.picture_ready(window, snapshot),
            Event::WarmCache => self.warm_cache(),
            Event::WarmWindows(targets) => self.warm_windows(targets),
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
            self.desktop = Some(desktop);
        }
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

    /// Recaptures the windows that are on screen right now, just before they start moving, so the window
    /// being typed in does not animate out with content from the previous switch.
    ///
    /// Does NOT help the window being switched INTO, which is not on screen yet. See "A picture is not
    /// the window" in `docs/capture-overlay-research.md`.
    fn refresh_visible(
        &mut self,
        windows: &[CanvasWindow],
        depths: &HashMap<u32, usize>,
        display: CGRect,
        scale: f64,
    ) {
        let started = Instant::now();
        let on_screen = crate::sys::window_server::visible_windows_on_display(display);

        // Frontmost first. The window at the front is the one being switched INTO, and it is the one
        // whose picture matters most: focus has already moved to it, so a picture taken before the
        // switch shows the app's unfocused rendering. Ghostty greys out when it is not focused, so the
        // tile slid in grey and snapped to black at the handover.
        let mut order: Vec<&CanvasWindow> = windows.iter().collect();
        order.sort_by_key(|window| depths.get(&window.server_id.as_u32()).copied().unwrap_or(usize::MAX));

        let mut attempts = 0usize;
        let mut refreshed = 0usize;
        for window in order {
            if attempts >= MAX_FRESH_CAPTURES {
                break;
            }
            // Its CURRENT size, from the window server where known. A canvas frame is in canvas
            // coordinates and says nothing about where the window is on screen at this moment. Not being
            // listed is not disqualifying: the destination workspace's windows have only just been
            // shown, and those are exactly the ones worth recapturing.
            let size = match on_screen.iter().find(|(id, _)| *id == window.server_id) {
                Some((_, frame)) => {
                    if on_screen_fraction(*frame, display) < FRESH_CAPTURE_MIN_ON_SCREEN {
                        continue;
                    }
                    frame.size
                }
                None => window.frame.size,
            };
            attempts += 1;
            let Some(snapshot) =
                capture_via_skylight(window.server_id, (size.width, size.height), scale)
            else {
                continue;
            };
            // A clipped or wrongly shaped capture is worse than a stale one, and `insert` already
            // refuses to replace a usable picture with an unusable one.
            if snapshot.is_usable() && snapshot.fits(window.frame.size) {
                self.cache.insert(window.window, snapshot);
                refreshed += 1;
            }
        }
        if attempts > 0 {
            debug!(
                attempts,
                refreshed,
                took_ms = started.elapsed().as_millis(),
                "recaptured on-screen windows, frontmost first, so they do not animate stale or unfocused"
            );
        }
    }

    /// Recaptures the window being switched into, now that it is on screen, and swaps its tile.
    ///
    /// Runs once per movement. By this point the reactor has shown the destination workspace and moved
    /// focus, so a SkyLight capture gets the app's FOCUSED rendering, which is what the real window will
    /// look like when the overlay lifts. Without this the tile slides in with whatever the picture held,
    /// and an app that dims when unfocused visibly snaps at the handover.
    ///
    /// Costs one capture on the main thread, measured at 32ms to 35ms warm, so it drops a frame or two
    /// mid-flight. That is the trade: one hitch against a step change in brightness on the window being
    /// looked at.
    fn refresh_destination(&mut self) {
        let Some((display, scale)) = self.display else { return };
        let Some(canvas) = self.canvas.as_ref() else { return };
        let candidates: Vec<(WindowId, u32)> =
            canvas.tiles.iter().map(|(w, s, _)| (*w, s.as_u32())).collect();
        if candidates.is_empty() {
            return;
        }
        let depths = crate::sys::window_server::front_to_back_depths();
        let on_screen: Vec<u32> = crate::sys::window_server::visible_windows_on_display(display)
            .into_iter()
            .filter(|(_, frame)| on_screen_fraction(*frame, display) >= FRESH_CAPTURE_MIN_ON_SCREEN)
            .map(|(id, _)| id.as_u32())
            .collect();
        let wanted = refresh_order(&candidates, &depths, &on_screen, MAX_DESTINATION_CAPTURES);

        for window in wanted {
            let Some((_, server_id, size)) =
                self.canvas.as_ref().and_then(|c| c.tiles.iter().find(|(w, _, _)| *w == window).copied())
            else {
                continue;
            };
            // On its own thread. A capture of a large window measured 38ms to 179ms, which on the main
            // thread dropped up to four frames of a 494ms flight. yabai captures on per-window pthreads
            // too (`window_manager.c:666`), so the call is safe off the main thread. The picture arrives
            // as an event a few frames later, which is fine: it replaces contents, not geometry.
            let tx = self.tx.clone();
            std::thread::Builder::new()
                .name("destination-recapture".to_string())
                .spawn(move || {
                    let started = Instant::now();
                    let Some(snapshot) =
                        capture_via_skylight(server_id, (size.width, size.height), scale)
                    else {
                        return;
                    };
                    if !snapshot.is_usable() || !snapshot.fits(size) {
                        return;
                    }
                    debug!(
                        idx = window.idx.get(),
                        took_ms = started.elapsed().as_millis(),
                        "recaptured the window being switched into, off the main thread"
                    );
                    _ = tx.send(Event::PictureReady { window, snapshot });
                })
                .ok();
        }
    }

    /// Takes a mid-flight recapture and swaps it into the tile that is already on screen.
    fn picture_ready(&mut self, window: WindowId, snapshot: WindowSnapshot) {
        self.cache.insert(window, snapshot.clone());
        // Only worth drawing while the movement that asked for it is still running.
        if self.canvas.is_none() {
            return;
        }
        if let Some(overlay) = self.overlay.as_mut() {
            overlay.set_tile_picture(window, &snapshot);
        }
    }

    fn refresh_snapshot(&mut self, window: WindowId, server_id: WindowServerId, size: CGSize) {
        let scale = self.display.map(|(_, scale)| scale).unwrap_or(2.0);
        if let Some(snapshot) = capture_via_skylight(server_id, (size.width, size.height), scale) {
            self.cache.insert(window, snapshot);
        }
    }

    /// The snapshot to draw for one window, from the cache only.
    ///
    /// A window with nothing cached does not animate: it is placed at its destination when the overlay
    /// comes down, and a background capture is queued so the next switch has it.
    fn snapshot_for(&mut self, request: &AnimationRequest) -> Option<WindowSnapshot> {
        // Rejected here rather than drawn distorted, for the same reason as the canvas path: contents
        // stretch to fill, so a picture of the wrong shape warps the window instead of moving it.
        self.cache
            .usable(request.window)
            .filter(|snapshot| snapshot.fits(request.to.size))
            .cloned()
    }

    /// Does this window appear on screen at ANY point during the animation?
    ///
    /// The whole path is sampled, not just its ends: a window that sweeps across mid-animation is
    /// exactly what conveys how far the strip travelled, and testing endpoints alone excluded it.
    fn is_worth_animating(&self, from: CGRect, to: CGRect, display: CGRect) -> bool {
        /// Fraction of the window that must be on screen at some sampled moment. Low, because a window
        /// crossing the display is only partly on it for most of the crossing.
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

    /// Queues windows for a background capture once the current animation finishes, keeping whatever is
    /// already queued rather than replacing it.
    fn remember_for_warming(&mut self, windows: Vec<AnimationRequest>) {
        for request in windows {
            if self.last_animated.iter().any(|target| target.window == request.window) {
                continue;
            }
            self.last_animated.push(SnapshotTarget {
                window: request.window,
                server_id: request.server_id,
                size: request.to.size,
            });
        }
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

        // A canvas movement already covers this ground and owns the tile layers, so this path must not
        // touch them. Its destinations still matter, since a later pass can move a window, so they are
        // merged into the canvas instead. See docs/capture-overlay-research.md.
        if let Some(canvas) = self.canvas.as_mut() {
            for (window, frame) in &final_frames {
                match canvas.final_frames.iter_mut().find(|(w, _)| w == window) {
                    Some(existing) => existing.1 = *frame,
                    None => canvas.final_frames.push((*window, *frame)),
                }
            }
            // The destinations changed, so anything already placed is stale. Ask again.
            canvas.frames_applied = false;
            debug!(
                windows = windows.len(),
                "a canvas movement is in flight; merging destinations into it rather than drawing"
            );
            self.remember_for_warming(windows);
            return;
        }

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

        // Merge into the animation in flight rather than replacing it: the reactor lays a layout out
        // over several passes, and a later pass can also change where a window is going.
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
        if let Some((display_frame, scale)) = self.display {
            self.refresh_visible(&windows, &depths, display_frame, scale);
        }
        let mut tiles = Vec::with_capacity(windows.len());
        let mut missing = 0usize;
        let mut misshapen = 0usize;
        for window in &windows {
            // Two questions, both of which must be yes: does the picture cover the window it was taken
            // from, and does it still match the frame it is drawn into.
            match self.cache.usable(window.window).cloned() {
                Some(snapshot) if snapshot.fits(window.frame.size) => tiles.push(CanvasTile {
                    window: window.window,
                    frame: window.frame,
                    snapshot,
                    depth: depths
                        .get(&window.server_id.as_u32())
                        .copied()
                        .unwrap_or(usize::MAX / 2),
                }),
                Some(snapshot) => {
                    misshapen += 1;
                    debug!(
                        pid = window.window.pid,
                        idx = window.window.idx.get(),
                        frame = format!(
                            "{:.0}x{:.0}",
                            window.frame.size.width, window.frame.size.height
                        ),
                        picture = format!(
                            "{:.0}x{:.0}",
                            snapshot.coverage.covered.0, snapshot.coverage.covered.1
                        ),
                        "not drawing a window whose picture is the wrong shape for its frame"
                    );
                }
                None => missing += 1,
            }
        }
        for tile in tiles.iter().take(4) {
            debug!(
                pid = tile.window.pid,
                idx = tile.window.idx.get(),
                frame = format!(
                    "{:.0},{:.0} {:.0}x{:.0}",
                    tile.frame.origin.x, tile.frame.origin.y,
                    tile.frame.size.width, tile.frame.size.height
                ),
                covered = format!(
                    "{:.0}x{:.0}",
                    tile.snapshot.coverage.covered.0, tile.snapshot.coverage.covered.1
                ),
                window = format!(
                    "{:.0}x{:.0}",
                    tile.snapshot.coverage.window.0, tile.snapshot.coverage.window.1
                ),
                source = format!("{:?}", tile.snapshot.source),
                "canvas tile"
            );
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
        let strip = self.bar_strip();

        let Some(overlay) = self.ensure_overlay() else {
            self.request_frames(final_frames);
            return;
        };
        overlay.set_backdrop(backdrop.as_ref());
        // The same picture as the backdrop, clipped to the bar. Drawing the bar from a separate capture
        // put a second, misaligned copy on screen.
        overlay.set_foreground(backdrop.as_ref(), strip);
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
            frames: 0,
            tiles: tiles
                .iter()
                .map(|tile| (tile.window, WindowServerId::from(tile.window), tile.frame.size))
                .collect(),
            destination_refreshed: false,
            _clock: clock,
        });

        // With no clock nothing would advance, so land it immediately.
        if self.canvas.as_ref().is_some_and(|c| c._clock.is_none()) {
            self.step_canvas_to_end();
        }
    }

    fn step_canvas(&mut self) {
        let (done, place_now, refresh_now) = {
            let Self { overlay, canvas, .. } = self;
            let Some(canvas) = canvas.as_mut() else { return };
            let progress = canvas.progress();
            let eased = crate::ui::workspace_overlay::ease_out_cubic(progress);
            if let Some(overlay) = overlay.as_mut() {
                overlay.set_canvas_offset(canvas.offset_at(eased));
                canvas.frames += 1;
            }
            let place_now = !canvas.frames_applied && progress >= APPLY_FRAMES_AT;
            if place_now {
                canvas.frames_applied = true;
            }
            let refresh_now = !canvas.destination_refreshed && progress >= REFRESH_DESTINATION_AT;
            if refresh_now {
                canvas.destination_refreshed = true;
            }
            (progress >= 1.0, place_now, refresh_now)
        };
        if refresh_now {
            self.refresh_destination();
        }
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
        if let Some(canvas) = self.canvas.as_ref() {
            debug!(
                frames = canvas.frames,
                elapsed_ms = canvas.started.elapsed().as_millis(),
                duration_ms = canvas.duration.as_millis(),
                travel = format!(
                    "{:.0},{:.0} -> {:.0},{:.0}",
                    canvas.from_offset.x,
                    canvas.from_offset.y,
                    canvas.to_offset.x,
                    canvas.to_offset.y
                ),
                "canvas movement finished"
            );
        }
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
    /// Asks for a fresh desktop capture in the background.
    ///
    /// Cheap to call: the service ignores the request when one is already in flight, and the desktop
    /// changes rarely enough that a capture a few seconds old is indistinguishable from a fresh one.
    fn warm_desktop(&self) {
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
        let desktop = crate::sys::window_server::desktop_backdrop_windows(display_frame);
        let composite = crate::ui::window_snapshot::capture_composite_via_skylight(
            &desktop.windows,
            display_size,
            scale,
        );

        let usable = composite.filter(|snapshot| {
            crate::ui::window_snapshot::is_backdrop_worth_drawing(
                self.has_backdrop || self.desktop.is_some(),
                desktop.has_wallpaper,
                snapshot.coverage.covered,
                display_size,
            )
        });

        if let Some(snapshot) = usable {
            self.has_backdrop = true;
            return Some(snapshot);
        }

        // Keep the render current for the next switch, whether or not one is in hand for this one.
        self.warm_desktop();
        match self.desktop.clone() {
            Some(rendered) => {
                self.has_backdrop = true;
                Some(rendered)
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

    /// Where the bar sits, in the overlay's own coordinates, or `None` when there is no bar.
    ///
    /// A rect only. The pixels come from the desktop capture, which already contains the bar, so nothing
    /// is captured separately: sketchybar is dozens of small windows and a composite of them covers only
    /// their union, which cannot be aligned against the copy already in the backdrop.
    fn bar_strip(&self) -> Option<CGRect> {
        let (display_frame, _) = self.display?;
        let bounds = crate::sys::window_server::bar_strip(display_frame).bounds?;
        Some(CGRect::new(
            CGPoint::new(
                bounds.origin.x - display_frame.origin.x,
                bounds.origin.y - display_frame.origin.y,
            ),
            bounds.size,
        ))
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

    mod refresh {
        use super::*;

        fn wid(idx: u32) -> WindowId {
            WindowId { pid: 1, idx: std::num::NonZeroU32::new(idx).unwrap() }
        }

        fn depths(pairs: &[(u32, usize)]) -> HashMap<u32, usize> {
            pairs.iter().copied().collect()
        }

        /// Frontmost first, because the front window is the one being switched into and the only one
        /// whose unfocused rendering is worth paying a capture to correct.
        #[test]
        fn orders_frontmost_first() {
            let candidates = [(wid(10), 10), (wid(20), 20), (wid(30), 30)];
            let order = refresh_order(
                &candidates,
                &depths(&[(10, 5), (20, 0), (30, 2)]),
                &[10, 20, 30],
                3,
            );
            assert_eq!(order, vec![wid(20), wid(30), wid(10)]);
        }

        #[test]
        fn takes_only_as_many_as_asked_for() {
            // Each capture costs a frame, so the cap is what keeps the hitch bounded.
            let candidates = [(wid(10), 10), (wid(20), 20), (wid(30), 30)];
            let order =
                refresh_order(&candidates, &depths(&[(10, 2), (20, 0), (30, 1)]), &[10, 20, 30], 1);
            assert_eq!(order, vec![wid(20)]);
        }

        #[test]
        fn skips_windows_that_are_not_on_screen() {
            // SkyLight reads the framebuffer, so capturing one of these would return a sliver and be
            // rejected anyway, after paying for it.
            let candidates = [(wid(10), 10), (wid(20), 20)];
            let order = refresh_order(&candidates, &depths(&[(10, 0), (20, 1)]), &[20], 2);
            assert_eq!(order, vec![wid(20)]);
        }

        #[test]
        fn is_empty_when_nothing_is_on_screen() {
            let candidates = [(wid(10), 10)];
            assert!(refresh_order(&candidates, &depths(&[(10, 0)]), &[], 2).is_empty());
        }

        #[test]
        fn a_window_with_no_known_depth_sorts_last_rather_than_first() {
            // Unknown depth must not outrank a window the window server actually reported as frontmost.
            let candidates = [(wid(10), 10), (wid(20), 20)];
            let order = refresh_order(&candidates, &depths(&[(20, 3)]), &[10, 20], 1);
            assert_eq!(order, vec![wid(20)]);
        }
    }

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
