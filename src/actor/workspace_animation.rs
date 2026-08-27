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
//! Canvas movements (workspace switches and strip pans) are drawn by Core Animation; the tick loop
//! only paces their mid-flight orchestration. The per-window path still draws tile by tile on the
//! ticks. See `docs/animation-smoothness.md`.
//!
//! Measurements behind all of this are in `docs/capture-overlay-research.md`.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_foundation::MainThreadMarker;
use tracing::{debug, warn};

use crate::actor;
use crate::actor::app::WindowId;
use crate::sys::geometry::SameAs;
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
    /// Held still while the canvas moves under it.
    ///
    /// A floating window does not belong to the strip, so a strip scroll must not carry it along. It does
    /// belong to a workspace, so a switch between workspaces DOES move it, and that path leaves this false.
    pub pinned: bool,
    /// Off the strip, and so in the other z-order group. Separate from `pinned`, which is about whether the
    /// canvas carries the window along: a workspace switch moves floating windows without unpinning them.
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
    /// Move the viewport across a canvas holding every window involved, rather than moving each window
    /// separately, so a long jump scrolls past everything in between instead of cutting to the
    /// destination.
    AnimateCanvas {
        windows: Vec<CanvasWindow>,
        from_offset: CGPoint,
        to_offset: CGPoint,
        /// Real screen frames to apply once the overlay is covering them.
        final_frames: Vec<(WindowId, CGRect)>,
        /// The window that will hold focus once this settles, drawn in front of the rest.
        focus: Option<WindowId>,
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

/// Tick interval. Canvas movements draw through Core Animation and use ticks only for mid-flight
/// orchestration; the per-window path still draws on them. 60fps rather than 120 because a prior
/// engine at 120 dropped frames and read worse than a slower rate that always lands.
const FRAME_INTERVAL: Duration = Duration::from_micros(16_667);

/// How long to keep collecting windows before the animation starts moving.
///
/// The reactor arranges a layout over several passes, and treating each as its own animation restarted
/// the motion. The overlay goes up immediately and the clock starts once the passes settle, so windows
/// joining in between cannot pop. One frame is enough and is imperceptible.
const COALESCE_WINDOW: Duration = Duration::from_millis(25);

/// How far into a movement to recapture the window being switched into.
///
/// Early, so the corrected picture is on screen for most of the flight, but not on the very first frame,
/// because the destination workspace's windows are only shown once the reactor has acted on the switch.
const REFRESH_DESTINATION_AT: f64 = 0.0;

/// A second attempt, later in the flight. An app repaints as focused on its own schedule, and the
/// first attempt can land before it has: the tile then still slides in dimmed. Still before the real
/// windows are placed, so the corrected picture is on screen before the handover.
const REFRESH_DESTINATION_AGAIN_AT: f64 = 0.5;

/// How many windows to recapture mid-flight. One: this exists for the window being switched into, and
/// each capture costs a frame.
const MAX_DESTINATION_CAPTURES: usize = 1;

/// How long after an animation to recapture the bar.
///
/// A bar composite measures 31ms median, so it cannot be paid at the start of a switch. Long enough after
/// the overlay hides that the compositor has dropped it out of the framebuffer, and long enough that a
/// burst of switches only pays it once, at the end.
const BAR_REFRESH_DELAY: Duration = Duration::from_millis(250);

/// Which of `candidates` to recapture, frontmost first, capped at `max`.
///
/// Frontmost first because the front window is the one being switched INTO, and the one whose picture
/// matters most. Capped because each capture costs a frame.
fn refresh_order(
    candidates: &[(WindowId, u32)],
    depths: &HashMap<u32, usize>,
    on_screen: Option<&[u32]>,
    max: usize,
) -> Vec<WindowId> {
    let mut ordered: Vec<&(WindowId, u32)> = candidates
        .iter()
        .filter(|(_, wsid)| on_screen.is_none_or(|ids| ids.contains(wsid)))
        .collect();
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
    /// Orchestration ticks that ran. Not drawn frames — the render server owns those and does not
    /// report them — so this only tells whether the tick loop had room for the mid-flight work.
    frames: u32,
    /// The windows on the canvas, with their server ids and drawn sizes, so the one being switched into
    /// can be found again mid-flight.
    tiles: Vec<(WindowId, WindowServerId, CGSize)>,
    /// How many times the window being switched into has been recaptured.
    ///
    /// It cannot be recaptured when the movement starts, because the destination workspace's windows are
    /// not on screen yet, so its picture is whatever it last had. If that was taken while the app was
    /// unfocused, the tile slides in dimmed and snaps to focused at the handover.
    destination_refreshed: u8,
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
    /// How many times the window being focused has been recaptured. See `refresh_destination_among`.
    destination_refreshed: u8,
    /// Dropped when the animation ends, which invalidates the timer and stops the wakeups.
    _clock: Option<RepeatingTimer>,
}

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
                existing.depth = tile.depth;
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
    /// A canvas movement in flight, which supersedes the per-window path while it runs.
    canvas: Option<RunningCanvas>,
    /// Fires once after the layout passes settle, to start the animation moving.
    coalesce: Option<RepeatingTimer>,
    /// Windows from the most recent animation, so the post-animation refresh uses real ids.
    last_animated: Vec<SnapshotTarget>,
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
            canvas: None,
            coalesce: None,
            last_animated: Vec::new(),
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
            Event::AnimateCanvas {
                windows,
                from_offset,
                to_offset,
                final_frames,
                focus,
                duration,
            } => {
                self.start_canvas(windows, from_offset, to_offset, final_frames, focus, duration)
            }
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
            Event::RefreshBar => {
                // One shot: dropping the timer stops it repeating.
                self.bar_refresh = None;
                self.refresh_bar();
            }
            Event::SnapshotsReady => self.collect_snapshots(),
            Event::PictureReady { window, snapshot } => self.picture_ready(window, snapshot),
            Event::WarmCache => self.warm_cache(),
            Event::WarmWindows(targets) => self.warm_windows(targets),
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
            let running = self.canvas.is_some() || self.running.is_some();
            self.cache.insert(window, snapshot.clone());
            // Straight onto the tile when an animation is mid-flight. That is how a ScreenCaptureKit
            // capture requested for a clipped destination reaches the screen before the handover.
            if running && snapshot.is_usable() {
                if let Some(overlay) = self.overlay.as_mut() {
                    overlay.set_tile_picture(window, &snapshot);
                }
            }
        }
    }

    /// Queues background captures for a set of windows the reactor identified.
    ///
    /// Already-held windows are skipped by the service, and the cache keeps what it has unless
    /// something better arrives, so calling this after every switch settles rather than re-capturing.
    fn warm_windows(&mut self, targets: Vec<SnapshotTarget>) {
        let wanted: Vec<SnapshotTarget> = targets
            .into_iter()
            // Drawable is not enough: the picture also has to match the size the window is now. A window
            // resized from 859pt to 1147pt keeps a perfectly usable 859pt picture, and this used to skip
            // it forever, so it was dropped from every animation as the wrong shape and visibly vanished
            // for the length of each one. `target.size` is the size the layout just gave it.
            .filter(|target| {
                crate::ui::window_snapshot::needs_capture(
                    self.cache.usable(target.window).map(|snapshot| snapshot.coverage),
                    (target.size.width, target.size.height),
                )
            })
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
        let Some(canvas) = self.canvas.as_ref() else { return };
        let tiles = canvas.tiles.clone();
        self.refresh_destination_among(&tiles);
    }

    /// Recaptures the frontmost of `tiles`, whichever drawing path is running.
    ///
    /// Both paths need this. A workspace switch animates the canvas, but focusing an adjacent window on
    /// a strip goes through the per-window path, and that is the case where the flicker is most obvious
    /// because both windows are the same app side by side.
    fn refresh_destination_among(&mut self, tiles: &[(WindowId, WindowServerId, CGSize)]) {
        let Some((_, scale)) = self.display else { return };
        let candidates: Vec<(WindowId, u32)> =
            tiles.iter().map(|(w, s, _)| (*w, s.as_u32())).collect();
        if candidates.is_empty() {
            return;
        }
        // No visibility filter here, unlike the pre-flight refresh. During a slide the destination is
        // mid-scroll and only partly on screen, so requiring it to be fully visible skipped it entirely,
        // which is why switching between adjacent windows still arrived unfocused. A clipped capture is
        // rejected below anyway; the cost of trying is one background thread.
        let depths = crate::sys::window_server::front_to_back_depths();
        let wanted = refresh_order(&candidates, &depths, None, MAX_DESTINATION_CAPTURES);
        let fully_visible: Vec<u32> = match self.display {
            Some((display, _)) => crate::sys::window_server::visible_windows_on_display(display)
                .into_iter()
                .filter(|(_, frame)| on_screen_fraction(*frame, display) >= FRESH_CAPTURE_MIN_ON_SCREEN)
                .map(|(id, _)| id.as_u32())
                .collect(),
            None => Vec::new(),
        };

        for window in wanted {
            let Some((_, server_id, size)) =
                tiles.iter().find(|(w, _, _)| *w == window).copied()
            else {
                continue;
            };
            // On its own thread. A capture of a large window measured 38ms to 179ms, which on the main
            // thread dropped up to four frames of a 494ms flight. yabai captures on per-window pthreads
            // too (`window_manager.c:666`), so the call is safe off the main thread. The picture arrives
            // as an event a few frames later, which is fine: it replaces contents, not geometry.
            // Both routes, always. ScreenCaptureKit works whatever the window's visibility but takes
            // long enough that it can miss the handover on a short flight; SkyLight is fast but can only
            // serve a window that is fully on screen, and during a slide the destination is mid-scroll.
            // Racing them means whichever can answer does, and a later arrival overwrites an earlier one.
            self.service.request(vec![SnapshotTarget { window, server_id, size }]);
            if !fully_visible.contains(&server_id.as_u32()) {
                continue;
            }
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
        // Only worth drawing while the animation that asked for it is still running.
        if self.canvas.is_none() && self.running.is_none() {
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
        /// Samples along the path. Enough that a window cannot cross the display between two of them:
        /// the fastest realistic travel is a few display widths, so eleven samples leave any crossing
        /// window on screen for at least one of them.
        const SAMPLES: usize = 11;

        let area = from.size.width * from.size.height;
        if area <= 0.0 {
            return false;
        }
        let enough = min_on_screen(is_moving(from, to));
        (0..SAMPLES).any(|step| {
            let t = step as f64 / (SAMPLES - 1) as f64;
            let at = crate::ui::workspace_overlay::lerp_rect(from, to, t);
            on_screen_fraction(at, display) >= enough
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
        let focused_group = focus_group(focus, windows.iter().map(|r| (r.window, r.floating)));

        let mut tiles = Vec::with_capacity(windows.len());
        let mut skipped = 0usize;
        let mut offscreen = 0usize;
        let mut needs_capture: Vec<SnapshotTarget> = Vec::new();
        for request in &windows {
            let start = actual_start(request, display_frame);
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
                    depth: crate::model::z_group::tile_depth(
                        depths.get(&request.server_id.as_u32()).copied(),
                        focus == Some(request.window),
                        group_of(request.floating),
                        focused_group,
                    ),
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
        if self.running.is_some() {
            let in_flight;
            let mut retargets: Vec<(WindowId, CGRect, usize)> = Vec::new();
            let mut joined: Vec<OverlayTile> = Vec::new();
            {
                let running = self.running.as_mut().expect("checked above");
                in_flight = running.started.is_some();
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
                    let copy = tile.clone();
                    match running.merge(tile) {
                        // The common rapid-press case: a later pass confirming destinations the
                        // flight already has. Touching nothing is what keeps chained presses from
                        // restarting or extending the animation forever.
                        Admitted::Redundant => {}
                        Admitted::Retargeted => retargets.push((copy.window, copy.to, copy.depth)),
                        Admitted::Joined => joined.push(copy),
                    }
                }
            }
            let changed = !(retargets.is_empty() && joined.is_empty());
            if in_flight {
                // Tiles already animating bend toward their new targets from wherever they are
                // drawn; newcomers start their whole movement now. Both get the fresh duration.
                if let Some(overlay) = self.overlay.as_mut() {
                    for (window, to, depth) in retargets {
                        overlay.retarget_tile(window, to, depth, duration);
                    }
                    for tile in &joined {
                        overlay.add_tile(tile, duration);
                    }
                }
                if changed {
                    let running = self.running.as_mut().expect("checked above");
                    // A real change restarts the orchestration clock so the frame placement and
                    // the teardown cover the flights that just began; without this the overlay
                    // lifts while a retargeted tile is still travelling.
                    running.started = Some(Instant::now());
                    running.duration = duration;
                    // The destinations changed, so the frames already requested are stale. Ask
                    // again once the animation is far enough along.
                    running.frames_applied = false;
                }
            } else {
                // Still collecting behind the coalesce window: compose statically at frame zero,
                // exactly as a fresh start does. The animations are installed once by
                // `start_moving`.
                let tiles = {
                    let running = self.running.as_mut().expect("checked above");
                    std::mem::take(&mut running.tiles)
                };
                if let Some(overlay) = self.overlay.as_mut() {
                    overlay.set_tiles(&tiles);
                    overlay.draw_frame(&tiles, 0.0);
                }
                if let Some(running) = self.running.as_mut() {
                    running.tiles = tiles;
                }
            }
            return;
        }

        // The same backdrop and bar as a canvas movement, or the overlay shows a bare black window behind
        // the tiles. Cheap for the same reason: the cached render is a clone.
        let held = self.pictures.shown.is_some();
        let backdrop = self.capture_backdrop().or_else(|| self.pictures.shown.clone());
        if backdrop.is_some() {
            self.pictures.shown = backdrop.clone();
        }
        let (bar, strip) = self.bar_picture();
        Self::log_dressing("per-window", backdrop.as_ref(), bar.as_ref(), strip, held);
        let Some(overlay) = self.ensure_overlay() else { return };
        overlay.set_backdrop(backdrop.as_ref());
        overlay.set_bar(bar.as_ref(), strip);
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
            destination_refreshed: 0,
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
        let focused_group = focus_group(focus, windows.iter().map(|w| (w.window, w.floating)));
        let mut tiles = Vec::with_capacity(windows.len());
        let mut missing = 0usize;
        let mut misshapen = 0usize;
        for window in &windows {
            // Two questions, both of which must be yes: does the picture cover the window it was taken
            // from, and does it still match the frame it is drawn into.
            match self.cache.usable(window.window).cloned() {
                Some(snapshot) => {
                    // A picture of the wrong shape is stretched to the frame rather than dropped. Dropping
                    // it left a hole the size of a window in an opaque overlay, so the window appeared to
                    // vanish for the whole animation, which is far worse than 350ms of a stretched picture.
                    // It should be rare: `warm_windows` recaptures anything whose picture no longer fits.
                    if !snapshot.fits(window.frame.size) {
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
                            "stretching a picture that is the wrong shape for its frame"
                        );
                    }
                    tiles.push(CanvasTile {
                        window: window.window,
                        frame: window.frame,
                        snapshot,
                        depth: crate::model::z_group::tile_depth(
                            depths.get(&window.server_id.as_u32()).copied(),
                            focus == Some(window.window),
                            group_of(window.floating),
                            focused_group,
                        ),
                        pinned: window.pinned,
                    });
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
                // The presentation tree says where the canvas is drawn right now; model
                // arithmetic covers the gap before the first presentation frame exists.
                let current = self
                    .overlay
                    .as_ref()
                    .and_then(|overlay| overlay.current_canvas_offset())
                    .unwrap_or_else(|| {
                        let eased =
                            crate::ui::workspace_overlay::ease_out_cubic(running.progress());
                        running.offset_at(eased)
                    });
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

        // Cheap in the steady state: `capture_backdrop` hands back the cached render, which is a clone and
        // an asynchronous refresh request. It only pays for a composite before the first render lands.
        // Holding the previous picture instead meant the cold-start composite was kept forever, so the
        // render that matches the real desktop never got its turn.
        let backdrop = self.capture_backdrop();
        let (bar, strip) = self.bar_picture();

        let Some(overlay) = self.ensure_overlay() else {
            self.request_frames(final_frames);
            return;
        };
        let held = self.pictures.shown.is_some();
        if backdrop.is_some() {
            self.pictures.shown = backdrop.clone();
        }
        Self::log_dressing("canvas", backdrop.as_ref(), bar.as_ref(), strip, held);
        let Some(overlay) = self.overlay.as_mut() else { return };
        overlay.set_backdrop(backdrop.as_ref());
        overlay.set_bar(bar.as_ref(), strip);
        overlay.set_canvas(&tiles);
        overlay.set_canvas_offset(from_offset);
        overlay.show();
        // Committed in the same run loop turn as the show, so the first presented frame is the
        // animation's first frame at `from_offset` — no gap for the destination to flash through.
        overlay.animate_canvas_offset(from_offset, to_offset, duration);

        // Ticks pace the mid-flight work in `step_canvas` only; the drawing is Core Animation's.
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
            destination_refreshed: 0,
            _clock: clock,
        });

        // With no clock the drawing would still run, but the real windows would never be placed
        // and the overlay would never come down, so land it immediately instead.
        if self.canvas.as_ref().is_some_and(|c| c._clock.is_none()) {
            self.step_canvas_to_end();
        }
    }

    fn step_canvas(&mut self) {
        let (done, place_now, refresh_now) = {
            let Some(canvas) = self.canvas.as_mut() else { return };
            let progress = canvas.progress();
            // Nothing is drawn here; a late tick delays a recapture or the frame placement,
            // never the motion.
            canvas.frames += 1;
            let place_now = !canvas.frames_applied && progress >= APPLY_FRAMES_AT;
            if place_now {
                canvas.frames_applied = true;
            }
            let refresh_now = match canvas.destination_refreshed {
                0 => progress >= REFRESH_DESTINATION_AT,
                1 => progress >= REFRESH_DESTINATION_AGAIN_AT,
                _ => false,
            };
            if refresh_now {
                canvas.destination_refreshed += 1;
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
                ticks = canvas.frames,
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
        self.arm_bar_refresh();
        let targets = std::mem::take(&mut self.last_animated);
        if !targets.is_empty() {
            self.warm_windows(targets);
        }
    }

    /// Starts an animation that is on screen but not yet moving: the clock, and the movements.
    ///
    /// This is the moment the tiles are handed to Core Animation, all in one transaction, so the
    /// passes collected behind the coalesce window travel as one group from one beat.
    fn start_moving(&mut self) {
        // Dropping the timer stops it repeating; it only ever needed to fire once.
        self.coalesce = None;
        let Some(running) = self.running.as_mut() else { return };
        if running.started.is_some() {
            return;
        }
        debug!(windows = running.tiles.len(), "starting the animation after coalescing");
        running.started = Some(Instant::now());
        let tiles = std::mem::take(&mut running.tiles);
        let duration = running.duration;
        if let Some(overlay) = self.overlay.as_mut() {
            overlay.animate_tiles(&tiles, duration);
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
            let place_now = !running.frames_applied && progress >= APPLY_FRAMES_AT;
            if place_now {
                running.frames_applied = true;
            }
            let refresh_now = match running.destination_refreshed {
                0 => progress >= REFRESH_DESTINATION_AT,
                1 => progress >= REFRESH_DESTINATION_AGAIN_AT,
                _ => false,
            };
            if refresh_now {
                running.destination_refreshed += 1;
            }
            (running.is_done(), place_now, refresh_now)
        };
        if refresh_now {
            let tiles: Vec<(WindowId, WindowServerId, CGSize)> = self
                .running
                .as_ref()
                .map(|running| {
                    running
                        .tiles
                        .iter()
                        .map(|tile| (tile.window, WindowServerId::from(tile.window), tile.to.size))
                        .collect()
                })
                .unwrap_or_default();
            self.refresh_destination_among(&tiles);
        }
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

        // Keep the render current whether or not one is in hand for this switch.
        self.warm_desktop();

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
        self.arm_bar_refresh();

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
fn actual_start(request: &AnimationRequest, display: CGRect) -> CGRect {
    let real = match crate::sys::window_server::get_window(request.server_id) {
        Some(info) if info.frame.size.width > 0.0 && info.frame.size.height > 0.0 => info.frame,
        _ => request.from,
    };
    // A parked window's real frame is a corner of the display, so animating from it would fly the window in
    // diagonally from the bottom. It belongs to the strip and comes back in from the edge it was parked
    // against.
    if crate::model::HiddenWindowPlacement::is_off_screen(display, real) {
        return crate::model::HiddenWindowPlacement::entry_frame(real, request.to, display);
    }
    real
}

/// How much of a window has to be on screen for the overlay to bother with it.
///
/// A window being moved needs a real share, or every parked sliver becomes a tile. A window standing still
/// needs only to be visible at all, because whatever shows of it turns into wallpaper otherwise.
fn min_on_screen(moving: bool) -> f64 {
    if moving { 0.25 } else { f64::MIN_POSITIVE }
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
/// Falls back to the strip when the focus target is not among the windows being animated, since that is
/// where focus lands for every movement the strip itself makes.
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
                Some(&[10, 20, 30]),
                3,
            );
            assert_eq!(order, vec![wid(20), wid(30), wid(10)]);
        }

        #[test]
        fn takes_only_as_many_as_asked_for() {
            // Each capture costs a frame, so the cap is what keeps the hitch bounded.
            let candidates = [(wid(10), 10), (wid(20), 20), (wid(30), 30)];
            let order =
                refresh_order(&candidates, &depths(&[(10, 2), (20, 0), (30, 1)]), Some(&[10, 20, 30]), 1);
            assert_eq!(order, vec![wid(20)]);
        }

        #[test]
        fn skips_windows_that_are_not_on_screen() {
            // SkyLight reads the framebuffer, so capturing one of these would return a sliver and be
            // rejected anyway, after paying for it.
            let candidates = [(wid(10), 10), (wid(20), 20)];
            let order = refresh_order(&candidates, &depths(&[(10, 0), (20, 1)]), Some(&[20]), 2);
            assert_eq!(order, vec![wid(20)]);
        }

        /// The destination path passes None, because during a horizontal slide the window being switched
        /// into is mid-scroll and would fail a visibility test that it should not be subject to.
        #[test]
        fn no_filter_considers_everything() {
            let candidates = [(wid(10), 10), (wid(20), 20)];
            let order = refresh_order(&candidates, &depths(&[(10, 1), (20, 0)]), None, 2);
            assert_eq!(order, vec![wid(20), wid(10)]);
        }

        #[test]
        fn is_empty_when_nothing_is_on_screen() {
            let candidates = [(wid(10), 10)];
            assert!(refresh_order(&candidates, &depths(&[(10, 0)]), Some(&[]), 2).is_empty());
        }

        #[test]
        fn a_window_with_no_known_depth_sorts_last_rather_than_first() {
            // Unknown depth must not outrank a window the window server actually reported as frontmost.
            let candidates = [(wid(10), 10), (wid(20), 20)];
            let order = refresh_order(&candidates, &depths(&[(20, 3)]), Some(&[10, 20]), 1);
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
            destination_refreshed: 0,
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
            destination_refreshed: 0,
            _clock: None,
        };
        assert!(running.progress() < 0.2, "just started");

        let finished = RunningAnimation {
            tiles: Vec::new(),
            final_frames: Vec::new(),
            frames_applied: false,
            started: Some(Instant::now() - Duration::from_secs(5)),
            duration: Duration::from_millis(180),
            destination_refreshed: 0,
            _clock: None,
        };
        // Clamped rather than allowed past 1.0, since the easing would otherwise overshoot the
        // target position when a frame arrives late.
        assert_eq!(finished.progress(), 1.0);
        assert!(finished.is_done());
    }
}
