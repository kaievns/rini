//! How the flight engine composes a request into a pass: a layout pass request by request, each
//! window on the picture it has or entering as a window that just opened does, and a strip
//! movement, each window on its picture or the stand-in.

use std::time::{Duration, Instant};

use rini_core::ids::{WindowId, WindowServerId};
use rini_geometry::space::{FromCg, GroupRect, Respace, ScreenRect, SurfacePoint};
use rini_geometry::{SameAs, from_overlay_space, to_overlay_space};
use tracing::debug;

use super::logged::Whole;
use super::tiles::{Stacking, frames_of, live_frame};
use super::{Composed, FlightEngine, Shape, restack};
use crate::animation::domain::choreography::{Cue, Look};
use crate::animation::domain::motion::accents::Accent;
use crate::animation::domain::motion::plan;
use crate::animation::domain::motion::surface::{SurfaceGeometry, pan_travel};
use crate::animation::platform::overlay::OverlayTile;
use crate::pictures::domain::route::Route;
use crate::pictures::domain::schedule::MAX_SYNC_ENTRANCE_CAPTURES;
use crate::pictures::domain::stand_in::picture_or_stand_in;
use crate::pictures::domain::target::PictureTarget;
use crate::pictures::platform::picture::Picture;
use crate::transitions::domain::admission::{
    EntranceDecision, FliesOn, PendingEntrance, Waits, entrance_plan,
};
use crate::transitions::domain::flight::worth_flying;
use crate::transitions::domain::lifecycle::{Kind, Start, TileSummary, drawn_focused};
use crate::transitions::domain::request::{AnimationRequest, SurfaceWindow};
use crate::transitions::domain::travel::{TilePath, is_moving, tile_path};

/// What every request of a layout pass is composed against: the display and its scale, the focus
/// its tiles are drawn with and the window drawn losing it, and the window server's depths and
/// radii.
struct PassReads {
    display: ScreenRect,
    scale: f64,
    focus: (Option<WindowId>, Option<WindowId>),
    stacking: Stacking,
}

/// A layout pass's tiles as its requests are composed, one by one.
#[derive(Default)]
struct PassTiles {
    tiles: Vec<OverlayTile>,
    /// Each window drawn from where its tile starts to where it ends, for the pass's plan.
    resolved: Vec<(WindowId, ScreenRect, ScreenRect, bool)>,
    /// Newly opened windows travelling from their spawn frame.
    spawn_entrances: Vec<(WindowId, GroupRect, GroupRect)>,
    /// Windows drawn once a first picture of them is swapped in.
    entrances: Vec<PendingEntrance>,
    /// Windows with no picture, asked for the next flight.
    needs_capture: Vec<PictureTarget>,
    waits: Waits,
    offscreen: usize,
    reserved: usize,
    /// Framed captures taken of windows that just opened, up to `MAX_SYNC_ENTRANCE_CAPTURES`.
    sync_captures: usize,
}

impl FlightEngine {
    /// A layout pass: each window's tile from where it is to where the pass puts it, on the picture
    /// it has (`compose_tile`), the pass posed as frame zero or merged into the flight running,
    /// unless nothing drawable moves and no flight runs.
    pub(super) fn start(
        &mut self,
        windows: Vec<AnimationRequest>,
        (focus, focused_before): (Option<WindowId>, Option<WindowId>),
        key: Option<WindowId>,
        duration: Duration,
        look: Look,
    ) {
        if windows.is_empty() {
            return;
        }
        let Some((display_frame, scale)) = self.display else {
            debug!("no display geometry yet; skipping animation");
            return;
        };

        // Every moving window's destination, picture or not. A still window is not re-placed: the
        // round trip invites another layout pass.
        let final_frames: Vec<(WindowId, ScreenRect)> = windows
            .iter()
            .filter(|request| is_moving(request.from, request.to))
            .map(|request| (request.window, request.to))
            .collect();
        let shape = Shape {
            kind: Kind::LayoutPass,
            start: Start::Coalesced,
            duration,
            waits: Waits::default(),
        };
        let requested = || {
            windows
                .iter()
                .map(|request| TileSummary {
                    window: request.window,
                    start: request.from,
                    end: request.to,
                    floating: request.floating,
                })
                .collect()
        };
        if !self.begin(requested, &final_frames, (focus, focused_before), key, &shape) {
            return;
        }
        let ids: Vec<WindowServerId> = windows.iter().map(|r| r.server_id).collect();
        let pass = PassReads {
            display: display_frame,
            scale,
            focus: self.drawing_focus((focus, focused_before)),
            stacking: Stacking::read(&*self.window_server, &ids),
        };
        let PassTiles {
            mut tiles,
            resolved,
            spawn_entrances,
            entrances,
            needs_capture,
            waits,
            offscreen,
            reserved,
            ..
        } = self.compose_requests(&windows, &pass);
        restack(&mut tiles, pass.focus.0);
        self.ask_missing(needs_capture);
        debug!(
            requested = windows.len(),
            tiles = tiles.len(),
            offscreen,
            no_snapshot = reserved,
            display = %Whole(display_frame),
            "overlay animation composition"
        );

        let moving_drawable = tiles.iter().any(|tile| is_moving(tile.from, tile.to));
        if !worth_flying(moving_drawable, self.running.is_some()) {
            self.abandon();
            return;
        }

        let mut plan = plan::reflow_plan(&resolved, display_frame);
        plan.entrances.extend(spawn_entrances);

        self.begin_group(Composed {
            tiles,
            final_frames,
            label: "per-window",
            shape: Shape { waits, ..shape },
            entrances,
            focus: (focus, focused_before),
            key,
            pan: None,
            plan,
            look,
            cue: Cue::Plain,
            accent: None,
        });
    }

    /// Every window of a layout pass where its tile starts and ends, but one not drawn on screen at
    /// either: its tile, or how it enters, and what the pass waits for of it.
    fn compose_requests(&self, windows: &[AnimationRequest], pass: &PassReads) -> PassTiles {
        let mut composed = PassTiles::default();
        for (index, request) in windows.iter().enumerate() {
            let live = live_frame(&*self.window_server, request.server_id);
            let path = tile_path(live, windows, index, pass.display);
            let Some(TilePath { start, end }) = path else {
                composed.offscreen += 1;
                debug!(
                    wsid = request.server_id.as_u32(),
                    from = %Whole(request.from),
                    to = %Whole(request.to),
                    "skipped as off screen"
                );
                continue;
            };
            let flies = self.compose_tile(request, (start, end), pass, &mut composed);
            let landing = flies.ends_at(end.size).and_then(|to| self.landing(request.window, to));
            composed.waits.push_request(request, flies, landing, start, pass.display);
        }
        composed
    }

    /// One window of a layout pass, its tile drawn from `start` to `end`: on any usable picture,
    /// whatever its shape, else the stand-in for a known window off this display, and with neither
    /// as it enters (`compose_entrance`). What it flies on.
    fn compose_tile(
        &self,
        request: &AnimationRequest,
        (start, end): (ScreenRect, ScreenRect),
        pass: &PassReads,
        composed: &mut PassTiles,
    ) -> FliesOn {
        let ws = &*self.window_server;
        // Any usable picture, whatever its shape; a wrong-shaped one is drawn cropped. See
        // "Resizes through the overlay" in `src/animation/docs/animation-smoothness.md`.
        let snapshot = self.pictures.usable(request.window);
        if snapshot.is_none() {
            // Asked for now so the next switch has pixels, even if this one does not.
            composed.needs_capture.push(PictureTarget {
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
        // leaving a hole; see `stands_in`. The flight holds for it, and its picture replaces the
        // stand-in once it has repainted where it ends.
        let snapshot = picture_or_stand_in(
            snapshot,
            || ws.get_window(request.server_id).map(|info| ScreenRect::from_cg(info.frame)),
            pass.display,
            || self.pictures.stand_in(request.to.size),
        );
        // No picture: almost always a window that just opened. See "A window that opens" in
        // `src/transitions/docs/coordination.md`.
        let Some(snapshot) = snapshot else {
            return FliesOn::Entrance(self.compose_entrance(request, pass, composed));
        };
        let flies = FliesOn::Picture {
            source: snapshot.source,
            covered: snapshot.coverage.covered,
        };
        let clipped = snapshot.source.clips();
        let (focus, losing) = pass.focus;
        let focused = drawn_focused(request.window, focus, losing);
        let tile = pass.stacking.tile_of(
            request.window,
            request.server_id,
            request.floating,
            request.worn,
        );
        let path = (
            to_overlay_space(start, pass.display),
            to_overlay_space(end, pass.display),
        );
        composed.tiles.push(tile.flying(path, snapshot, clipped, focused));
        composed.resolved.push((request.window, start, end, request.floating));
        flies
    }

    /// How a window of a layout pass with no picture enters: from the frame the window server shows
    /// it at on this display, drawn with one framed capture there, at most
    /// `MAX_SYNC_ENTRANCE_CAPTURES` a pass, a still tile if that is its slot; else reserved until a
    /// first picture of it lands. What was decided.
    fn compose_entrance(
        &self,
        request: &AnimationRequest,
        pass: &PassReads,
        composed: &mut PassTiles,
    ) -> EntranceDecision {
        let ws = &*self.window_server;
        // Only a frame on this display counts as a spawn; capturing off screen is slow.
        let spawn = ws
            .get_window(request.server_id)
            .map(|info| ScreenRect::from_cg(info.frame))
            .filter(|f| !rini_geometry::is_off_screen(pass.display, *f));
        let budget_left = composed.sync_captures < MAX_SYNC_ENTRANCE_CAPTURES;
        let captured_at = Instant::now();
        let picture = spawn
            .filter(|f| f.size.width > 0.0 && f.size.height > 0.0 && budget_left)
            .and_then(|_| {
                composed.sync_captures += 1;
                (self.capture)(ws, request.server_id, pass.scale)
            });
        let usable = picture.filter(Picture::is_usable);
        let entrance = entrance_plan(spawn, request.to, usable, budget_left);
        let decided = entrance.decided();
        match entrance {
            EntranceDecision::Travel { from, to, picture: snapshot } => {
                debug!(
                    pid = request.window.pid,
                    idx = request.window.idx.get(),
                    spawn = %Whole(from),
                    slot = %Whole(to),
                    capture_ms = captured_at.elapsed().as_millis() as u64,
                    "entrance from spawn"
                );
                self.pictures.insert(request.window, snapshot.clone());
                let clipped = snapshot.source.clips();
                let from_o = to_overlay_space(from, pass.display);
                let to_o = to_overlay_space(to, pass.display);
                // Already at its slot (a cold cache, not an open): a still tile.
                let standing = from.same_as(to);
                let (focus, losing) = pass.focus;
                let focused = !standing || drawn_focused(request.window, focus, losing);
                let mut tile = pass.stacking.tile_of(
                    request.window,
                    request.server_id,
                    request.floating,
                    request.worn,
                );
                if !standing {
                    tile.server_order = Some(0);
                }
                composed.tiles.push(tile.flying((from_o, to_o), snapshot, clipped, focused));
                if standing {
                    composed.resolved.push((request.window, from, to, request.floating));
                } else {
                    let entrance = (plan::installed(from_o), plan::installed(to_o));
                    composed.spawn_entrances.push((request.window, entrance.0, entrance.1));
                }
            }
            EntranceDecision::Reserve(reason) => {
                composed.reserved += 1;
                debug!(
                    pid = request.window.pid,
                    idx = request.window.idx.get(),
                    reason,
                    "entrance reserved"
                );
                composed.entrances.push(PendingEntrance {
                    window: request.window,
                    to: to_overlay_space(request.to, pass.display),
                    floating: request.floating,
                    worn: request.worn,
                });
            }
        }
        decided
    }

    /// Animates the whole strip surface as one rigid group, one container and one position
    /// animation, with a container of its own for each column a move swapped that crosses it, and a
    /// move's step (`accent`) stepping every strip container. A bounce from rest is one with no
    /// travel. See "The move flight, staged" in `src/transitions/docs/coordination.md`.
    #[allow(clippy::too_many_arguments, reason = "no roadmap task groups them")]
    pub(super) fn start_surface(
        &mut self,
        windows: Vec<SurfaceWindow>,
        from_offset: SurfacePoint,
        to_offset: SurfacePoint,
        final_frames: Vec<(WindowId, ScreenRect)>,
        focus: (Option<WindowId>, Option<WindowId>),
        key: Option<WindowId>,
        duration: Duration,
        (look, cue, accent): (Look, Cue, Option<Accent>),
        kind: Option<Kind>,
    ) {
        let Some((display, _)) = self.display else {
            debug!("no display geometry yet; skipping animation");
            return;
        };
        let shape = Shape {
            kind: kind.unwrap_or_else(|| Kind::of_surface(&windows, from_offset, to_offset)),
            start: Start::Immediate,
            duration,
            waits: Waits::default(),
        };
        let requested = || {
            windows
                .iter()
                .map(|w| {
                    let (from, to) = w.travel(from_offset, to_offset);
                    TileSummary {
                        window: w.window,
                        start: from_overlay_space(from, display),
                        end: from_overlay_space(to, display),
                        floating: w.floating,
                    }
                })
                .collect()
        };
        if !self.begin(requested, &final_frames, focus, key, &shape) {
            return;
        }
        let (tiles, waits) = self.surface_tiles(&windows, (from_offset, to_offset), display, focus);

        // One rigid piece for the strip and one for each column a move swapped that crosses it.
        let drawn: Vec<SurfaceWindow> = windows
            .iter()
            .filter(|w| tiles.iter().any(|t| t.window == w.window))
            .cloned()
            .collect();
        let plan = plan::surface_plan(&drawn, from_offset, to_offset);

        // A strip movement never resizes, never carries a brand-new window and never holds for
        // itself: a stand-in whose window comes on screen is pictured once it lands there.
        self.begin_group(Composed {
            tiles,
            final_frames,
            label: "surface",
            shape: Shape { waits, ..shape },
            entrances: Vec::new(),
            focus,
            key,
            pan: Some(pan_travel(from_offset, to_offset)),
            plan,
            look,
            cue,
            accent,
        });
    }

    /// Each window of a strip movement travelling from `from_offset` to `to_offset`, banded, on its
    /// picture, else the stand-in for a known window parked off `display`, and with neither left to
    /// `final_frames`; and what the movement waits for of each.
    fn surface_tiles(
        &self,
        windows: &[SurfaceWindow],
        (from_offset, to_offset): (SurfacePoint, SurfacePoint),
        display: ScreenRect,
        focus: (Option<WindowId>, Option<WindowId>),
    ) -> (Vec<OverlayTile>, Waits) {
        let (drawing_focus, losing) = self.drawing_focus(focus);

        // One query for every window, not one each: a round trip per window stalled this thread
        // for 100-200ms a press. See "Rapid presses" in `specs/focus.md`.
        let ids: Vec<WindowServerId> = windows.iter().map(|w| w.server_id).collect();
        let stacking = Stacking::read(&*self.window_server, &ids);
        let real_frames = frames_of(&*self.window_server, &ids);
        let mut tiles = Vec::with_capacity(windows.len());
        let mut waits = Waits::default();
        let mut missing = 0usize;
        let mut misshapen = 0usize;
        let mut stand_ins = 0usize;
        for window in windows {
            let (from, to) = window.travel(from_offset, to_offset);
            let cached = self.pictures.usable(window.window);
            // A known window parked off this display with no picture flies as the stand-in, as on
            // the per-window path; see `stands_in`.
            let snapshot = picture_or_stand_in(
                cached,
                || real_frames.get(&window.server_id).copied(),
                display,
                || self.pictures.stand_in(window.frame.size.respace()),
            );
            match snapshot {
                Some(snapshot) => {
                    // A wrong-shaped picture is drawn rather than dropped, cropped with its window's
                    // ground. See "A window that was resized keeps a usable picture" in
                    // `src/pictures/docs/capture-research.md`.
                    if snapshot.source == Route::StandIn {
                        stand_ins += 1;
                    } else if !snapshot.fits(window.frame.size) {
                        misshapen += 1;
                    }
                    waits.push_strip(window.window, self.landing(window.window, to.size.respace()));
                    let clipped = snapshot.source.clips();
                    let focused = drawn_focused(window.window, drawing_focus, losing);
                    let tile = stacking.tile_of(
                        window.window,
                        window.server_id,
                        window.floating,
                        window.worn,
                    );
                    tiles.push(tile.flying((from, to), snapshot, clipped, focused));
                }
                // Still placed by `final_frames`, and warmed once the movement settles.
                None => missing += 1,
            }
        }
        restack(&mut tiles, drawing_focus);
        debug!(
            requested = windows.len(),
            tiles = tiles.len(),
            missing,
            misshapen,
            stand_ins,
            travel = format!("{} -> {}", Whole(from_offset), Whole(to_offset)),
            "surface group animation"
        );
        (tiles, waits)
    }
}
