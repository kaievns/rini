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
use crate::ui::window_snapshot::{SnapshotCache, WindowSnapshot, capture_via_skylight};
use crate::ui::workspace_overlay::{OverlayTile, WorkspaceOverlay};

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
    /// One frame of the running animation. Posted by the run loop timer, not by any other actor.
    Tick,
}

pub type Sender = actor::Sender<Event>;
pub type Receiver = actor::Receiver<Event>;

/// Frame interval. 60fps rather than 120 because the previous engine was configured at 120 and the
/// per-frame work could not keep up, so frames were dropped and the result was less smooth than a
/// slower rate that always lands. Nothing here writes to another process, so this may be worth
/// revisiting once the overlay is doing the drawing.
const FRAME_INTERVAL: Duration = Duration::from_micros(16_667);

struct RunningAnimation {
    tiles: Vec<OverlayTile>,
    started: Instant,
    duration: Duration,
    /// Dropped when the animation ends, which invalidates the timer and stops the wakeups.
    _clock: Option<RepeatingTimer>,
}

impl RunningAnimation {
    /// Progress from the clock, not from a frame count, so a late frame skips ahead instead of
    /// stretching the animation.
    fn progress(&self) -> f64 {
        if self.duration.is_zero() {
            return 1.0;
        }
        let elapsed = self.started.elapsed().as_secs_f64();
        (elapsed / self.duration.as_secs_f64()).clamp(0.0, 1.0)
    }

    fn is_done(&self) -> bool {
        self.progress() >= 1.0
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
    display: Option<(CGRect, f64)>,
    running: Option<RunningAnimation>,
}

impl WorkspaceAnimation {
    pub fn new(rx: Receiver, tx: Sender, mtm: MainThreadMarker) -> Self {
        Self {
            rx,
            tx,
            mtm,
            overlay: None,
            cache: SnapshotCache::new(),
            display: None,
            running: None,
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
            Event::SetDisplay { frame, scale } => self.set_display(frame, scale),
            Event::Animate { windows, duration } => self.start(windows, duration),
            Event::RefreshSnapshot { window, server_id, size } => {
                self.refresh_snapshot(window, server_id, size)
            }
            Event::ForgetWindow(window) => self.cache.forget(window),
            Event::DebugSlide { dx, dy, duration } => self.debug_slide(dx, dy, duration),
            Event::Tick => self.step(),
        }
    }

    fn set_display(&mut self, frame: CGRect, scale: f64) {
        self.display = Some((frame, scale));
        if let Some(overlay) = self.overlay.as_mut() {
            overlay.set_frame(frame, scale);
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

    /// The snapshot to draw for one window: fresh pixels when they are worth having, cache otherwise.
    ///
    /// A fresh SkyLight capture is only usable when the window is fully on screen, because that call
    /// reads the framebuffer. When it comes back as a sliver the cached full-size capture is the
    /// better picture even though it is older, so it wins.
    fn snapshot_for(&mut self, request: &AnimationRequest) -> Option<WindowSnapshot> {
        let scale = self.display.map(|(_, scale)| scale).unwrap_or(2.0);
        let size = (request.from.size.width, request.from.size.height);
        let fresh = capture_via_skylight(request.server_id, size, scale);
        match &fresh {
            Some(snapshot) => debug!(
                wsid = request.server_id.as_u32(),
                covered = format!("{:.0}x{:.0}", snapshot.coverage.covered.0, snapshot.coverage.covered.1),
                window = format!("{:.0}x{:.0}", snapshot.coverage.window.0, snapshot.coverage.window.1),
                usable = snapshot.is_usable(),
                "skylight capture"
            ),
            None => debug!(wsid = request.server_id.as_u32(), "skylight capture returned nothing"),
        }
        if let Some(fresh) = fresh {
            let usable = fresh.is_usable();
            self.cache.insert(request.window, fresh);
            if usable {
                return self.cache.get(request.window).cloned();
            }
        }
        let fallback = self.cache.usable(request.window).cloned();
        if fallback.is_none() {
            debug!(wsid = request.server_id.as_u32(), "no usable snapshot, window left out");
        }
        fallback
    }

    fn start(&mut self, windows: Vec<AnimationRequest>, duration: Duration) {
        if windows.is_empty() {
            return;
        }
        let Some((display_frame, _)) = self.display else {
            debug!("no display geometry yet; skipping animation");
            return;
        };

        let mut tiles = Vec::with_capacity(windows.len());
        let mut skipped = 0usize;
        for request in &windows {
            match self.snapshot_for(request) {
                Some(snapshot) => tiles.push(OverlayTile {
                    window: request.window,
                    from: to_overlay_space(request.from, display_frame),
                    to: to_overlay_space(request.to, display_frame),
                    snapshot,
                }),
                // No usable picture. Better to leave the window out than to draw a smear: it will
                // simply appear at its destination when the overlay drops.
                None => skipped += 1,
            }
        }
        if skipped > 0 {
            debug!(skipped, total = windows.len(), "windows animated without a usable snapshot");
        }
        if tiles.is_empty() {
            return;
        }

        let Some(overlay) = self.ensure_overlay() else { return };
        overlay.set_tiles(&tiles);
        overlay.draw_frame(&tiles, 0.0);
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

        self.running =
            Some(RunningAnimation { tiles, started: Instant::now(), duration, _clock: clock });

        // With no clock the animation would never advance, so land it immediately rather than
        // leaving the overlay up over a frozen picture.
        if self.running.as_ref().is_some_and(|running| running._clock.is_none()) {
            self.step_to_end();
        }
    }

    fn step(&mut self) {
        let Some(running) = self.running.as_ref() else { return };
        let progress = running.progress();
        let done = running.is_done();

        if let Some(overlay) = self.overlay.as_ref() {
            overlay.draw_frame(&running.tiles, progress);
        }

        if done {
            self.finish();
        }
    }

    /// Jumps to the end and tears down, for the case where no frame clock could be created.
    fn step_to_end(&mut self) {
        if let (Some(overlay), Some(running)) = (self.overlay.as_ref(), self.running.as_ref()) {
            overlay.draw_frame(&running.tiles, 1.0);
        }
        self.finish();
    }

    fn finish(&mut self) {
        if let Some(overlay) = self.overlay.as_mut() {
            overlay.hide();
            // Free the bitmaps rather than sit on tens of MB of window pictures while idle.
            overlay.release_tiles();
        }
        // Dropping the animation drops its timer, which stops the wakeups.
        self.running = None;
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
                // A synthetic WindowId: the debug path never talks to the real window, and tiles are
                // only keyed by it, so any stable per-window value works.
                window: WindowId {
                    pid: 0,
                    idx: std::num::NonZeroU32::new(server_id.as_u32().max(1)).unwrap(),
                },
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
            started: Instant::now(),
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
            started: Instant::now(),
            duration: Duration::from_millis(180),
            _clock: None,
        };
        assert!(running.progress() < 0.2, "just started");

        let finished = RunningAnimation {
            tiles: Vec::new(),
            started: Instant::now() - Duration::from_secs(5),
            duration: Duration::from_millis(180),
            _clock: None,
        };
        // Clamped rather than allowed past 1.0, since the easing would otherwise overshoot the
        // target position when a frame arrives late.
        assert_eq!(finished.progress(), 1.0);
        assert!(finished.is_done());
    }
}
