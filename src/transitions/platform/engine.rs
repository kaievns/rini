//! The flight engine: the actor that carries out every transition. It holds the lifecycle with its
//! journal (`crate::transitions::domain::journal`'s `Journalled`, over
//! `crate::transitions::domain::lifecycle`'s `Coordinator`), stages each request and merges each
//! pass into the flight running, and does what the coordinator says: it hands the renderer
//! (`crate::animation::platform::renderer`) each event's render commands as one batch, among them the
//! edits of each flight's score (`crate::transitions::domain::flight_score`), holds what must wait
//! for a batch to be drawn until the renderer commits it (`crate::transitions::domain::drawn`), asks
//! and watches the pictures service (`crate::pictures::platform::service`), and sends each burst of
//! the journal to its writer (`crate::transitions::platform::journal`). When anything happens is the
//! coordinator's, but for the hide of a lift held, while the handover is measured, for the overlay's
//! own picture, which the journal decides (`crate::transitions::domain::journal::Journalled::heard`).
//! Runs on the `transitions` thread, made there from `Parts`, and touches no Core Animation and no
//! AppKit: the renderer, on the main thread, it reaches only through `Draw` and hears only as
//! `Event::Rendered`; every instant it reads is its `Clock`'s, and every begin time its
//! `MediaClock`'s.
//!
//! Design in `src/transitions/docs/coordination.md`; how a flight moves in
//! `src/animation/docs/animation-smoothness.md`; measurements in `src/pictures/docs/capture-research.md` and
//! `src/animation/docs/capture-overlay-research.md`.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use rini_core::ids::{WindowId, WindowServerId};
use rini_geometry::space::{
    FromCg, GroupRect, OverlayPoint, OverlayRect, OverlayVector, Point, PointSpace, Respace,
    ScreenRect, ScreenSize, Size, SurfacePoint,
};
use rini_geometry::stretch::grows;
use rini_geometry::{SameAs, from_overlay_space, to_overlay_space};
use rini_runloop::channel;
use rini_runloop::run_loop::RepeatingTimer;
use tracing::{debug, warn};

use crate::animation::domain::choreography::{Cue, Look, Pass, Stage};
use crate::animation::domain::motion::accents::Accent;
use crate::animation::domain::motion::plan::{self, entrance_from};
use crate::animation::domain::motion::score::{Edit, MediaClock, Node, Value};
use crate::animation::domain::motion::surface::{SurfaceGeometry, pan_travel};
use crate::animation::domain::motion::tile;
use crate::animation::platform::overlay::{OverlayTile, TRACE_TARGET, same_layout, window_origin};
use crate::animation::platform::renderer::{Batch, Render};
use crate::decorations::domain::style::Worn;
use crate::pictures::domain::handover::HandoverCheck;
use crate::pictures::domain::route::Route;
use crate::pictures::domain::schedule::{Ask, Display, Hold, MAX_SYNC_ENTRANCE_CAPTURES, Priority};
use crate::pictures::domain::stand_in::picture_or_stand_in;
use crate::pictures::domain::target::PictureTarget;
use crate::pictures::domain::watch::{
    CHANGE_EVERY, CHANGE_TRIES, Client, END_SLACK, LIVE_EVERY, Moment, OnScreen, Settle, Tag,
    Watch, WatchId, Why,
};
use crate::pictures::platform::picture::Picture;
use crate::pictures::platform::service::Pictures;
use crate::transitions::domain::admission::*;
use crate::transitions::domain::drawn::{Drawn, Owed};
use crate::transitions::domain::flight::*;
use crate::transitions::domain::flight_score::FlightScore;
use crate::transitions::domain::journal::{
    Heard, Journalled, OVERLAY_TAKE_PATIENCE, Wearing, measures,
};
use crate::transitions::domain::lifecycle::{
    self, Command, EndWatch, Generation, Kind, StageSummary, Start, TileSummary, drawn_focused,
    extend_latest, losing_focus,
};
use crate::transitions::domain::request::{AnimationRequest, Cause, SurfaceWindow};
use crate::transitions::domain::timing::*;
use crate::transitions::domain::travel::{TilePath, is_moving, tile_path};
use crate::transitions::platform::journal::Writer;
use crate::transitions::platform::logged::Whole;
use crate::transitions::settings::JournalSettings;
use crate::windows::platform::window_server::WindowServer;

mod tiles;

use tiles::{Stacking, entrance_tile};

/// What the engine is told.
#[derive(Debug)]
pub enum Event {
    /// Animate a set of windows: a layout pass. The engine writes their frames through `PlaceFrames`
    /// once the overlay covers them.
    Animate {
        windows: Vec<AnimationRequest>,
        focus: Option<WindowId>,
        /// The window focused as the pass began: what frame zero pictures focused.
        focused_before: Option<WindowId>,
        /// The key window as the pass went out, as far as the reactor has heard: whether frame zero
        /// pictures `focused_before` drawn focused.
        key: Option<WindowId>,
        duration: Duration,
        /// How the flight moves, if this pass begins one.
        look: Look,
        /// The event the pass answers, where its journal starts.
        cause: Cause,
    },
    /// Display geometry for the overlay: the display's whole bounds. Whether a bar under the
    /// overlay is drawn over flights is the pictures service's, which the reactor tells.
    SetDisplay {
        id: u32,
        frame: ScreenRect,
        scale: f64,
        /// Points to leave clear at the top for the bar; the overlay window starts below it.
        top_band: f64,
        /// Whether anything is drawn at rest on it, which a landed flight places under the overlay a
        /// tick ahead of its lift.
        rests: bool,
    },
    /// Move the whole strip surface by one travel, as one rigid group; a leaving window animates
    /// off screen while its real frame parks, and the columns a move swapped cross the surface from
    /// their old slots, but for the one passed when the strip steps, which is held on it. See
    /// "Strip movements" in `src/transitions/docs/coordination.md`.
    AnimateSurface {
        windows: Vec<SurfaceWindow>,
        from_offset: SurfacePoint,
        to_offset: SurfacePoint,
        /// Real frames to apply once the overlay is covering them.
        final_frames: Vec<(WindowId, ScreenRect)>,
        /// The window that will hold focus once this settles, drawn in front of the rest.
        focus: Option<WindowId>,
        /// The window focused as the movement began: what frame zero pictures focused.
        focused_before: Option<WindowId>,
        /// The key window as the movement went out, as for `Animate`.
        key: Option<WindowId>,
        duration: Duration,
        /// How the flight moves, if this movement begins one.
        look: Look,
        /// The press, for the look.
        cue: Cue,
        /// What rides the movement, as the look staged it: a move's step, the whole strip away from
        /// where the moved window went and back, drawn only.
        accent: Option<Accent>,
        /// The event the movement answers, where its journal starts.
        cause: Cause,
    },
    /// Nudge the strip surface by `overshoot` and bring it back; real windows stay put. Rides an
    /// in-flight movement additively. See "Edge bounce" in `src/transitions/docs/coordination.md`.
    Bounce {
        windows: Vec<SurfaceWindow>,
        overshoot: OverlayVector,
        final_frames: Vec<(WindowId, ScreenRect)>,
        focus: Option<WindowId>,
        duration: Duration,
        /// How the flight moves, if this push begins one.
        look: Look,
        /// The press, for the look.
        cue: Cue,
        /// The event the push answers, where its journal starts.
        cause: Cause,
    },
    /// One frame of the clock that runs from a transition's begin to its lift. Posted by its timer.
    Tick,
    /// The quiet after the lift of `Generation` ran out. Posted by the quiet timer.
    Quiet(Generation),
    /// The patience of a lift held for the overlay's own picture ran out. Posted by its timer.
    LiftPatience,
    /// The key window changed, which turned these windows' looks, each at the frame it is at. Sent by
    /// the reactor for every change it hears of, rini's own raises' included.
    LooksTurned(Vec<(WindowId, ScreenRect)>),
    /// What the pictures service landed. Posted by its sink.
    Pictures(crate::pictures::event::Event),
    /// Whether the journal is written and each lift's handover measured, which holds a lift's hide for
    /// the overlay's own picture. Sent by the reactor at start and at each config reload.
    Journal(JournalSettings),
    /// What the renderer made of the batches it drew last. Posted by its report.
    Rendered(crate::animation::event::Event),
    /// A turn of the main thread over a frame, for the journal. Sent by its run loop's observer.
    MainTurn(rini_runloop::turns::MainTurn),
}

impl Event {
    /// When the event a flight request answers was taken: none for anything but a request.
    pub fn pressed(&self) -> Option<Instant> {
        match self {
            Event::Animate { cause, .. }
            | Event::AnimateSurface { cause, .. }
            | Event::Bounce { cause, .. } => Some(cause.at),
            _ => None,
        }
    }
}

/// Hands the renderer a batch of render commands, which it answers as `Event::Rendered`.
pub type Draw = Box<dyn Fn(Batch) + Send>;

/// Called with real-window frames to apply while the overlay covers them.
pub type PlaceFrames = Box<dyn Fn(Vec<(WindowId, ScreenRect)>) + Send>;

/// Called with `true` when a transition is reported up and `false` once it has settled, the quiet
/// after its lift; the coordinator's `ReportUp` and `ReportSettled`.
pub type OnFlight = Box<dyn Fn(bool) + Send>;

/// The instant now.
pub type Clock = Box<dyn Fn() -> Instant + Send>;

/// One framed capture of a window where the window server has it, at a display's scale: what a
/// window that just opened flies from (`skylight::capture_via_framed`).
pub type Capture = Box<dyn Fn(&dyn WindowServer, WindowServerId, f64) -> Option<Picture> + Send>;

pub type Sender = channel::Sender<Event>;
pub type Receiver = channel::Receiver<Event>;

/// What the engine is made from: its queue, the pictures service as `client`, and how it reaches
/// everything else.
pub struct Parts {
    pub rx: Receiver,
    /// Timers post back into the engine's own queue through it.
    pub tx: Sender,
    /// Tells the engine what lands through its sink, as `Event::Pictures`.
    pub pictures: Pictures,
    /// The engine as the application numbers the service's clients.
    pub client: Client,
    pub draw: Draw,
    pub place_frames: PlaceFrames,
    pub on_flight: OnFlight,
    /// The renderer's media clock, which every track begins on.
    pub media: MediaClock,
    pub clock: Clock,
    /// Every frame, depth and corner radius the engine reads.
    pub window_server: Arc<dyn WindowServer>,
    pub capture: Capture,
}

/// What a flight composed: its tiles and the pictures they draw, its plan, the entrances still
/// waiting for a picture, and what the renderer has been handed. When any of it changes is the
/// coordinator's.
struct RunningAnimation {
    tiles: Vec<OverlayTile>,
    /// Where each window must end up, across every pass merged in.
    final_frames: Vec<(WindowId, ScreenRect)>,
    /// Windows drawn once a first picture of them is swapped in.
    entrances: Vec<PendingEntrance>,
    /// The window gaining focus, from the latest pass that named one; its group is banded in front.
    focus: Option<WindowId>,
    /// The window focus is drawn leaving until it hands over: the one focused as the flight began,
    /// then the target drawn focused.
    shown_focus: Option<WindowId>,
    /// The key window as the flight's first pass went out.
    key: Option<WindowId>,
    /// The focus target whose tile flies its focused look, until a picture is swapped onto it.
    focused_look: Option<WindowId>,
    /// The flight as rigid pieces, which its look composes the score from.
    /// See "The overlay engine" in `src/animation/docs/animation-smoothness.md`.
    plan: plan::FlightPlan,
    /// The plan is playing on the renderer: a pass or a picture now retargets it rather than
    /// installing frame zero again.
    flown: bool,
    /// Everything the flight draws, and the accents waiting for its motion.
    score: FlightScore,
    /// The press frame zero shows: the latest folded into it.
    cue: Cue,
    /// Passes that reached the flight while it moved, oldest first, not merged until the coordinator
    /// says (`Command::Retarget`).
    held: Vec<Composed>,
}

/// What a swapped-in picture became.
enum Took {
    /// A reserved entrance's tile, joining the flight.
    Entrance(Box<OverlayTile>),
    /// The picture on the window's tile.
    Tile,
    /// The picture its tile's leg in two steps flies from the corner on.
    Corner,
    /// Nothing draws the window.
    Nothing,
}

/// Writes every tile's destination back from the merged plan: a member the pass did not compose
/// rides its group all the same.
fn sync_tiles_to_plan(tiles: &mut [OverlayTile], plan: &plan::FlightPlan) {
    for tile in tiles.iter_mut() {
        if let Some(to) = plan.destination(tile.window) {
            tile.to = to;
        }
    }
}

/// The z rule's view of every tile, in tile order.
fn stacked_windows(
    tiles: &[OverlayTile],
) -> Vec<crate::animation::domain::motion::z_group::Stacked> {
    tiles
        .iter()
        .map(|t| crate::animation::domain::motion::z_group::Stacked {
            window: t.window,
            group: crate::windows::domain::raise_order::StackGroup::of(t.floating),
            server_order: t.server_order,
        })
        .collect()
}

/// Depth for every tile, banded by `z_group::stack`.
/// The reactor's regroup matches it. See "The overlay engine" in `src/animation/docs/animation-smoothness.md`.
fn restack(tiles: &mut [OverlayTile], focus: Option<WindowId>) {
    let placements =
        crate::animation::domain::motion::z_group::stack(&stacked_windows(tiles), focus);
    for (tile, placement) in tiles.iter_mut().zip(placements) {
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
    use crate::animation::domain::motion::z_group::{Band, focus_is_off_strip, stack};
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

/// What a pass says of itself beside its tiles: its kind, how it starts and runs, and which of its
/// windows are not painted where they end yet.
#[derive(Clone, Debug)]
struct Shape {
    kind: Kind,
    start: Start,
    duration: Duration,
    waits: Waits,
}

/// One pass, composed: what `begin_group` poses or merges.
struct Composed {
    tiles: Vec<OverlayTile>,
    final_frames: Vec<(WindowId, ScreenRect)>,
    label: &'static str,
    shape: Shape,
    entrances: Vec<PendingEntrance>,
    /// The focus it names, and the window focused as it began.
    focus: (Option<WindowId>, Option<WindowId>),
    /// The key window as it went out.
    key: Option<WindowId>,
    pan: Option<OverlayVector>,
    plan: plan::ReflowPlan,
    /// How a flight it begins moves; a pass merged into one is moved by that flight's.
    look: Look,
    cue: Cue,
    accent: Option<Accent>,
}

/// What the coordinator is told of a stage.
#[allow(clippy::too_many_arguments, reason = "no roadmap task groups them")]
fn summary(
    tiles: Vec<TileSummary>,
    final_frames: Vec<(WindowId, ScreenRect)>,
    focus: Option<WindowId>,
    losing_focus: Option<WindowId>,
    focused_look: Option<WindowId>,
    key: Option<WindowId>,
    shape: Shape,
    (display, rests): (ScreenRect, bool),
    opaque: &dyn Fn(WindowId) -> bool,
) -> StageSummary {
    let opaque = tiles.iter().map(|t| t.window).filter(|w| opaque(*w)).collect();
    StageSummary {
        kind: shape.kind,
        tiles,
        final_frames,
        focus,
        losing_focus,
        key,
        focused_look,
        awaiting: shape.waits.awaiting,
        repainting: shape.waits.repainting,
        kept: shape.waits.kept,
        start: shape.start,
        duration: shape.duration,
        display,
        rests,
        opaque,
    }
}

impl RunningAnimation {
    /// Every tile and every reserved entrance, as the coordinator sees them.
    fn tile_summaries(&self, display: ScreenRect) -> Vec<TileSummary> {
        let tiles = self.tiles.iter().map(|t| TileSummary {
            window: t.window,
            start: from_overlay_space(t.from, display),
            end: from_overlay_space(t.to, display),
            floating: t.floating,
        });
        let entrances = self.entrances.iter().map(|e| TileSummary {
            window: e.window,
            start: from_overlay_space(entrance_from(e.to), display),
            end: from_overlay_space(e.to, display),
            floating: e.floating,
        });
        tiles.chain(entrances).collect()
    }

    /// The flight as composed, for the coordinator.
    fn stage(
        &self,
        shape: Shape,
        (display, rests): (ScreenRect, bool),
        opaque: &dyn Fn(WindowId) -> bool,
    ) -> StageSummary {
        summary(
            self.tile_summaries(display),
            self.final_frames.clone(),
            self.focus,
            losing_focus(self.focus, self.shown_focus),
            self.focused_look,
            self.key,
            shape,
            (display, rests),
            opaque,
        )
    }

    /// Frame zero's pictures, each on its window's tile, clipped or whole; the windows whose tiles
    /// took one.
    fn wear(&mut self, pictures: Vec<(WindowId, Picture, bool)>) -> Vec<WindowId> {
        let mut worn = Vec::new();
        for (window, picture, clipped) in pictures {
            if let Some(tile) = self.tiles.iter_mut().find(|t| t.window == window) {
                tile.snapshot = picture;
                tile.clipped = clipped;
                worn.push(window);
            }
        }
        worn
    }

    /// What the journal records of the picture each of `windows`' tiles wears now.
    fn worn(&self, windows: &[WindowId]) -> Vec<(WindowId, Route, bool, Instant)> {
        self.tiles
            .iter()
            .filter(|tile| windows.contains(&tile.window))
            .map(|tile| {
                (
                    tile.window,
                    tile.snapshot.source,
                    tile.snapshot.carries_blur,
                    tile.snapshot.taken,
                )
            })
            .collect()
    }

    /// The tile a reserved entrance becomes with its first picture, `clipped` or whole and rounded at
    /// the radius `radius` reads, read only for an entrance, banded with the rest; `None` for a window
    /// not waiting to enter.
    fn admit(
        &mut self,
        window: WindowId,
        picture: &Picture,
        clipped: bool,
        radius: impl FnOnce() -> Option<f64>,
    ) -> Option<OverlayTile> {
        let at = self.entrances.iter().position(|e| e.window == window)?;
        let entrance = self.entrances.remove(at);
        self.merge(entrance_tile(&entrance, picture, clipped, radius()));
        restack(&mut self.tiles, self.focus);
        self.tiles.iter().find(|t| t.window == window).cloned()
    }

    /// A picture swapped in for `window`, `clipped` or whole, at `at` on the media clock: a reserved
    /// entrance joins with it, rounded at the radius `radius` reads; one waiting for the corner of
    /// its tile's leg in two steps is what the tile flies from there (`waits_for_corner`), clipped if
    /// it or the tile's own is; else its tile takes it in place, a stand-in's included, moving on as
    /// it was. Either way the tile flies no focused look from now.
    fn take_picture(
        &mut self,
        window: WindowId,
        picture: &Picture,
        clipped: bool,
        radius: impl FnOnce() -> Option<f64>,
        at: f64,
    ) -> Took {
        if let Some(tile) = self.admit(window, picture, clipped, radius) {
            return Took::Entrance(Box::new(tile));
        }
        let waits = self.waits_for_corner(window, picture, at);
        let Some(tile) = self.tiles.iter_mut().find(|t| t.window == window) else {
            return Took::Nothing;
        };
        if self.focused_look == Some(window) {
            self.focused_look = None;
        }
        if waits {
            tile.from_corner = Some(picture.clone());
            tile.clipped |= clipped;
            return Took::Corner;
        }
        tile.snapshot = picture.clone();
        tile.from_corner = None;
        tile.clipped = clipped;
        Took::Tile
    }

    /// Whether `picture`, landing for `window` at `at` on the media clock, waits for the corner of
    /// its tile's leg in two steps: one not fitting where that leg starts, before the motion on a leg
    /// in two steps (`tile::two_steps`), and once it moves until its leg turns the corner. One fitting
    /// the leg's start, as frame zero's does, goes on at once, and so does any onto a tile drawn
    /// stretched, with no crop grid to cut (`tile::content_mode`).
    fn waits_for_corner(&self, window: WindowId, picture: &Picture, at: f64) -> bool {
        let Some(tile) = self.tiles.iter().find(|t| t.window == window) else {
            return false;
        };
        let start = self
            .score
            .leg_start(window)
            .filter(|_| self.flown)
            .map_or(tile.from.size, Respace::respace);
        let (covered, source) = (tile.snapshot.coverage.covered, tile.snapshot.source);
        if tile::content_mode(covered, source, start, tile.to.size) == tile::ContentMode::Stretch
            || picture.fits(start)
        {
            return false;
        }
        if !self.flown {
            return tile::two_steps(tile.from, tile.to).is_some();
        }
        self.score.corner_at(window).is_some_and(|corner| at < corner)
    }

    /// Whether `tile`'s picture from the press covers a leg from where the score draws it at `at` to
    /// `to`: every frame of it, so a repaint held for a corner not reached by `at` is not needed.
    fn press_covers<U: PointSpace>(&self, tile: &OverlayTile, to: Size<U>, at: f64) -> bool {
        let press = tile.snapshot.coverage.covered;
        let drawn = self.score.drawn_size(tile.window, at);
        drawn.is_some_and(|drawn| !grows(press, drawn.respace()) && !grows(press, to.respace()))
    }

    /// Each picture a tile holds for a corner it no longer waits for at `at`, its leg in one movement
    /// now or past its corner, made the tile's own: called as a leg is made, so a new one starts
    /// from what its tile shows.
    fn turn_corners(&mut self, at: f64) {
        let waiting: Vec<WindowId> = self
            .tiles
            .iter()
            .filter(|tile| tile.from_corner.is_some())
            .map(|tile| tile.window)
            .filter(|window| !self.score.corner_at(*window).is_some_and(|corner| at < corner))
            .collect();
        for tile in self.tiles.iter_mut().filter(|tile| waiting.contains(&tile.window)) {
            if let Some(picture) = tile.from_corner.take() {
                tile.snapshot = picture;
            }
        }
    }

    /// Each picture held for a corner a merge at `at` bent its tile's leg away from before reaching
    /// it, let go where the picture from the press covers the new leg (`press_covers`): it was never
    /// drawn, and the tile flies on with no ground.
    fn drop_corners_covered(&mut self, at: f64) {
        let covered: Vec<WindowId> = self
            .tiles
            .iter()
            .filter(|tile| tile.from_corner.is_some() && self.press_covers(tile, tile.to.size, at))
            .map(|tile| tile.window)
            .collect();
        for tile in self.tiles.iter_mut().filter(|tile| covered.contains(&tile.window)) {
            tile.from_corner = None;
        }
    }

    /// The size of the picture `window`'s tile carries into a leg a merge at `at` bends to end `to`
    /// big: one held for its corner once that is past, or while the picture from the press does not
    /// cover the new leg; the tile's own otherwise, as `take_pass` leaves it.
    fn carried_into(&self, window: WindowId, to: ScreenSize, at: f64) -> Option<ScreenSize> {
        let tile = self.tiles.iter().find(|tile| tile.window == window)?;
        let ahead = self.score.corner_at(window).is_some_and(|corner| at < corner);
        let held = tile.from_corner.as_ref().filter(|_| !ahead || !self.press_covers(tile, to, at));
        Some(held.unwrap_or(&tile.snapshot).coverage.covered)
    }

    /// `target`'s tile flies `look`, drawn focused and `clipped` or whole, if it fits where the tile
    /// starts. Whether it does.
    fn fly_focused_look(&mut self, target: WindowId, look: Option<Picture>, clipped: bool) -> bool {
        let Some(tile) = self.tiles.iter_mut().find(|t| t.window == target) else {
            return false;
        };
        let Some(look) = look.filter(|look| look.fits(tile.from.size)) else {
            return false;
        };
        tile.snapshot = look;
        tile.clipped = clipped;
        tile.focused = true;
        self.focused_look = Some(target);
        true
    }

    /// `window`'s tile drawn focused or not from now; drawn focused, focus has handed over to it.
    fn draw_focused(&mut self, window: WindowId, focused: bool) {
        if focused {
            self.shown_focus = Some(window);
        }
        if let Some(tile) = self.tiles.iter_mut().find(|t| t.window == window) {
            tile.focused = focused;
        }
    }

    /// Folds one later pass into the flight's tiles and focus; the overlay follows `merge_plans`. A
    /// pass moving focus redraws which tiles are drawn focused, but for the window focus is drawn
    /// leaving, which stays as its own turn left it, focus moving back to it or on, and none flies a
    /// focused look until the engine gives the new target its own.
    fn merge_pass(
        &mut self,
        tiles: Vec<OverlayTile>,
        focus: Option<WindowId>,
    ) -> Vec<(WindowId, Admitted)> {
        let focus_moved = focus.is_some() && focus != self.focus;
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
        if focus_moved {
            let losing = losing_focus(self.focus, self.shown_focus);
            let shown = self.shown_focus;
            for tile in self.tiles.iter_mut().filter(|t| Some(t.window) != shown) {
                tile.focused = drawn_focused(tile.window, self.focus, losing);
            }
            self.focused_look = None;
        }
        restack(&mut self.tiles, self.focus);
        outcomes
    }

    /// A later pass reaching the flight. Before it moves, folded into frame zero by its look
    /// (`FlightScore::fold`), the accent it brings waiting for the motion: a burst composes as one
    /// movement from frame zero, which shows the pass's press, each tile starting where its container
    /// installs it, and frame zero is posed again (`None`). Once it moves, bent by its look from the
    /// score at `at`, the tiles following the merged plan: what the merge changed, and the edits that
    /// carry it out.
    fn take_pass(&mut self, pass: &Pass, at: f64) -> Option<(plan::PlanDelta, Vec<Edit>)> {
        if self.flown {
            self.turn_corners(at);
            let merged = self.score.merge(&self.plan, pass, at);
            self.plan = merged.plan;
            sync_tiles_to_plan(&mut self.tiles, &self.plan);
            self.drop_corners_covered(at);
            self.turn_corners(at);
            return Some((merged.delta, merged.edits));
        }
        if let Some(accent) = pass.accent {
            self.score.wait(accent);
        }
        self.plan = self.score.fold(&self.plan, pass);
        self.cue = pass.cue;
        for tile in self.tiles.iter_mut() {
            if let Some(member) = self.plan.member(tile.window) {
                tile.from = plan::on_overlay(member.start_frame());
            }
        }
        sync_tiles_to_plan(&mut self.tiles, &self.plan);
        None
    }

    /// A push against `wall` reaching the flight: waiting for the motion until the flight moves, the
    /// latest push winning (`None`), and played on the score at `at` once it does.
    fn push(&mut self, cue: &Cue, wall: OverlayVector, seconds: f64, at: f64) -> Option<Vec<Edit>> {
        if !self.flown {
            self.score.wait(Accent::Push { wall, seconds });
            return None;
        }
        Some(self.score.push(cue, wall, seconds, at))
    }

    /// The windows the flight draws a tile of.
    fn drawn(&self) -> Vec<WindowId> { self.tiles.iter().map(|tile| tile.window).collect() }

    /// How far the bands `window` wears at rest reach outside it on a display of `scale`, 0 with
    /// none: its border's, and its ring's if it wears the ring where the flight ends.
    fn band(&self, window: WindowId, scale: f64) -> f64 {
        let worn = self.tiles.iter().find(|tile| tile.window == window).map(|tile| tile.worn);
        worn.map_or(0.0, |worn| {
            crate::decorations::domain::shape::outset(
                &worn.decoration,
                worn.wears_ring.worn(),
                scale,
            )
        })
    }

    /// Each end the overlay is holed over, with the outset of the band its window wears at rest.
    fn holes(&self, ends: Vec<(WindowId, OverlayRect)>, scale: f64) -> Vec<(OverlayRect, f64)> {
        ends.into_iter().map(|(window, end)| (end, self.band(window, scale))).collect()
    }

    /// Adds or retargets one window without disturbing anything already moving, each tile already
    /// flying wearing what the pass ends with.
    fn merge(&mut self, tile: OverlayTile) -> Admitted {
        let action = merge_action(
            self.tiles.iter().find(|t| t.window == tile.window).map(|t| t.to),
            tile.to,
        );
        match action {
            Admitted::Redundant => {
                if let Some(existing) = self.tiles.iter_mut().find(|t| t.window == tile.window) {
                    existing.worn = tile.worn;
                }
            }
            Admitted::Retargeted => {
                let existing = self
                    .tiles
                    .iter_mut()
                    .find(|t| t.window == tile.window)
                    .expect("retarget implies the tile exists");
                // The original start is kept so a moving window is not yanked backwards, and the
                // picture and focus it is drawn with: a pass is composed from the cache, and a tile
                // changes its picture only for one the flight took.
                existing.to = tile.to;
                existing.floating = tile.floating;
                existing.server_order = tile.server_order;
                existing.depth = tile.depth;
                existing.worn = tile.worn;
            }
            Admitted::Joined => self.tiles.push(tile),
        }
        action
    }
}

/// What the pictures service is asked for `window` drawn at `frame`.
fn target_at(window: WindowId, frame: ScreenRect) -> PictureTarget {
    PictureTarget {
        window,
        server_id: WindowServerId::from(window),
        size: frame.size,
    }
}

fn targets_at(windows: &[(WindowId, ScreenRect)]) -> Vec<(PictureTarget, ScreenRect)> {
    windows
        .iter()
        .map(|&(window, frame)| (target_at(window, frame), frame))
        .collect()
}

fn server_ids(windows: &[WindowId]) -> Vec<WindowServerId> {
    windows.iter().map(|window| WindowServerId::from(*window)).collect()
}

/// The windows on screen at `strip` and `background`, asked for under `tag` at `moment`.
fn on_screen(
    tag: Tag,
    moment: Moment,
    strip: &[(WindowId, ScreenRect)],
    background: &[(WindowId, ScreenRect)],
    behind: &[WindowId],
) -> OnScreen {
    OnScreen {
        tag,
        moment,
        strip: targets_at(strip),
        background: targets_at(background),
        behind: server_ids(behind),
    }
}

/// Each window at the size of `frame`: what the quiet warms, at the frame each ended at, and what a
/// turned look is asked for at.
fn sized_targets(windows: Vec<(WindowId, ScreenRect)>) -> Vec<PictureTarget> {
    windows.into_iter().map(|(window, frame)| target_at(window, frame)).collect()
}

/// The end state's watches: one slot for each kind, kept from one generation to the next and
/// retargeted, so a window already landed stays landed and a change watch stays armed.
#[derive(Debug, Default, PartialEq)]
struct EndWatches {
    landing: Option<WatchId>,
    awaiting: Option<WatchId>,
    change: Option<WatchId>,
    live: Option<WatchId>,
}

impl EndWatches {
    /// Watches what `end` names, in place of what was watched, under `client`'s tag.
    fn watch(&mut self, pictures: &Pictures, client: Client, end: &EndWatch) {
        let tag = Tag {
            client,
            generation: end.generation.0,
        };
        let behind = server_ids(&end.behind);
        let landing = |windows: &[(WindowId, ScreenRect)], settle, until| {
            (!windows.is_empty()).then(|| Watch::Landing {
                tag,
                windows: targets_at(windows),
                behind: behind.clone(),
                slack: END_SLACK,
                settle,
                until,
            })
        };
        let watches = [
            landing(&end.landing, Settle::AsSoonAsThere, end.landing_until),
            landing(&end.awaiting, Settle::Repainted, end.awaiting_until),
            end.focus.map(|(window, frame)| Watch::Change {
                tag,
                window: target_at(window, frame),
                end: frame,
                losing: end.losing_focus.map(|(window, frame)| (target_at(window, frame), frame)),
                behind: behind.clone(),
                every: CHANGE_EVERY,
                tries: CHANGE_TRIES,
            }),
        ];
        for (slot, watch) in [&mut self.landing, &mut self.awaiting, &mut self.change]
            .into_iter()
            .zip(watches)
        {
            put(slot, watch, pictures);
        }
        let live = (!end.background.is_empty()).then(|| Watch::Live {
            tag,
            windows: end
                .background
                .iter()
                .map(|&(window, frame)| target_at(window, frame))
                .collect(),
            every: LIVE_EVERY,
            after: self.change,
            initial: end.background_initial,
        });
        put(&mut self.live, live, pictures);
    }

    fn stop(&mut self, pictures: &Pictures) {
        for slot in [
            &mut self.landing,
            &mut self.awaiting,
            &mut self.change,
            &mut self.live,
        ] {
            put(slot, None, pictures);
        }
    }
}

/// `watch` in `slot`: retargeting the watch there, starting one, or stopping it.
fn put(slot: &mut Option<WatchId>, watch: Option<Watch>, pictures: &Pictures) {
    match (*slot, watch) {
        (Some(id), Some(watch)) => pictures.retarget(id, watch),
        (None, Some(watch)) => *slot = Some(pictures.watch(watch)),
        (Some(id), None) => {
            pictures.stop(id);
            *slot = None;
        }
        (None, None) => {}
    }
}

/// A lift held, while the handover is measured, for the overlay's own picture: the patience's timer,
/// and what waits for the lift to be drawn once it hides.
struct HeldLift {
    _patience: RepeatingTimer,
    owed: Vec<Owed>,
}

pub struct FlightEngine {
    rx: Receiver,
    /// Timers post back into this actor's own queue through it.
    tx: Sender,
    draw: Draw,
    /// The render commands of the event being handled and the batch number they go under, handed to
    /// the renderer as one batch at its end.
    batch: Option<(u64, Vec<Render>)>,
    /// What waits for a batch to be drawn.
    drawn: Drawn,
    /// The batch the latest `Render::Show` went in, which the real frames are written after.
    shown: u64,
    /// The batch the latest `Render::Lift` went in, which `Settle` waits for.
    lifted: u64,
    /// The overlay's window as the window server numbers it, once the renderer has made it.
    overlay: Option<WindowServerId>,
    /// Every picture the flight draws, and the background captures that fill it.
    pictures: Pictures,
    /// This engine as the pictures service tags what it delivers.
    client: Client,
    display: Option<(ScreenRect, f64)>,
    /// Which display the overlay is on, for the pictures of it.
    display_id: Option<u32>,
    /// Points reserved at the top of the display for the bar. The overlay window starts below it,
    /// while its coordinates stay the full display's, so a captured desktop and the tiles keep their
    /// registration. Zero with the bar off.
    top_band: f64,
    /// Whether anything is drawn at rest on the display, as the last `SetDisplay` said.
    rests: bool,
    /// The latest press a request carried: a place or a lift given before a later one's is not that
    /// press's flight, which what is drawn at rest tells apart.
    took: Option<Instant>,
    /// When everything happens, and the journal of it.
    flights: Journalled,
    /// What the reactor last said of the journal; `None` until it has.
    journal: Option<JournalSettings>,
    /// Writes each burst `flights` gives while the journal is written.
    writer: Option<Writer>,
    running: Option<RunningAnimation>,
    /// The picture delivered of each window, under its generation and at its frame, while the
    /// coordinator holds it to swap in later (`Coordinator::holds`): a landing held back from over a
    /// focused look until focus hands over, and the target's change until the window focus leaves has
    /// turned. Every other delivery a tile takes as it comes or is left to the pictures service's
    /// cache.
    delivered: HashMap<WindowId, (Generation, ScreenRect, Picture)>,
    watches: EndWatches,
    /// The frame clock, from a transition's begin to its lift.
    frame_clock: Option<RepeatingTimer>,
    /// The quiet after the lift of a generation.
    quiet: Option<(Generation, RepeatingTimer)>,
    /// The end windows the last watch named ending wholly on screen: what a measured lift compares
    /// (`EndWatch::compared`).
    ends: Vec<(WindowId, ScreenRect)>,
    /// The lift held for the overlay's own picture, while the handover is measured.
    held: Option<HeldLift>,
    /// Places the real windows once the overlay covers them.
    place_frames: PlaceFrames,
    on_flight: OnFlight,
    media: MediaClock,
    clock: Clock,
    window_server: Arc<dyn WindowServer>,
    capture: Capture,
}

impl FlightEngine {
    pub fn new(parts: Parts) -> Self {
        let Parts {
            rx,
            tx,
            pictures,
            client,
            draw,
            place_frames,
            on_flight,
            media,
            clock,
            window_server,
            capture,
        } = parts;
        Self {
            rx,
            tx,
            draw,
            batch: None,
            drawn: Drawn::default(),
            shown: 0,
            lifted: 0,
            overlay: None,
            pictures,
            client,
            display: None,
            display_id: None,
            top_band: 0.0,
            rests: false,
            took: None,
            flights: Journalled::default(),
            journal: None,
            writer: None,
            running: None,
            delivered: HashMap::new(),
            watches: EndWatches::default(),
            frame_clock: None,
            quiet: None,
            ends: Vec::new(),
            held: None,
            place_frames,
            on_flight,
            media,
            clock,
            window_server,
            capture,
        }
    }

    pub async fn run(mut self) {
        while let Some((span, event)) = self.rx.recv().await {
            let _guard = span.enter();
            self.handle(event);
        }
    }

    /// One event. A request first hides a lift waiting for the overlay's picture, and gives the
    /// journal its press, for the request's first drive to mark. What the event draws goes to the
    /// renderer as one batch at its end, and what that waits for is carried out as its commit is heard.
    fn handle(&mut self, event: Event) {
        match event {
            Event::SetDisplay {
                id,
                frame,
                scale,
                top_band,
                rests,
            } => {
                self.rests = rests;
                self.set_display(id, frame, scale, top_band)
            }
            Event::Animate {
                windows,
                focus,
                focused_before,
                key,
                duration,
                look,
                cause,
            } => {
                self.take_request(cause);
                self.start(windows, (focus, focused_before), key, duration, look)
            }
            Event::AnimateSurface {
                windows,
                from_offset,
                to_offset,
                final_frames,
                focus,
                focused_before,
                key,
                duration,
                look,
                cue,
                accent,
                cause,
            } => {
                self.take_request(cause);
                self.start_surface(
                    windows,
                    from_offset,
                    to_offset,
                    final_frames,
                    (focus, focused_before),
                    key,
                    duration,
                    (look, cue, accent),
                    None,
                )
            }
            Event::Bounce {
                windows,
                overshoot,
                final_frames,
                focus,
                duration,
                look,
                cue,
                cause,
            } => {
                self.take_request(cause);
                self.start_bounce(windows, overshoot, final_frames, focus, duration, (look, cue))
            }
            Event::Tick => self.tick(),
            Event::Quiet(generation) => {
                if self.quiet.as_ref().is_some_and(|(armed, _)| *armed == generation) {
                    self.quiet = None;
                }
                self.drive(lifecycle::Event::Quiet(generation));
            }
            Event::LiftPatience => self.lift_heard(Heard::Patience),
            Event::Pictures(landed) => self.pictures_landed(landed),
            Event::LooksTurned(windows) => self.drive(lifecycle::Event::LooksTurned(windows)),
            Event::Journal(settings) => self.set_journal(settings),
            Event::Rendered(answer) => self.rendered(answer),
            Event::MainTurn(turn) => self.flights.main_turn(
                turn.began,
                turn.busy,
                &turn.actors,
                turn.outside,
                &turn.changes,
            ),
        }
        self.keep_held();
        self.send_batch();
        self.flights.answered();
    }

    fn now(&self) -> Instant { (self.clock)() }

    /// A request: a press never waits on a measurement.
    fn take_request(&mut self, cause: Cause) {
        self.lift_heard(Heard::Request);
        self.took = self.took.max(Some(cause.at));
        self.flights.pressed(cause);
    }

    /// Tells the coordinator `event`, carries out what it says, and sends the writer the burst due.
    fn drive(&mut self, event: lifecycle::Event) {
        let now = self.now();
        let (commands, burst) = self.flights.drive(event, now);
        for command in commands {
            self.carry_out(command, now);
        }
        if let (Some(burst), Some(writer)) = (burst, &self.writer) {
            writer.send(burst);
        }
    }

    /// The journal written or not, and each lift's handover measured or not, as the reactor says.
    /// Turned on, what the ring holds is written at once, or at the settle of the burst up.
    fn set_journal(&mut self, settings: JournalSettings) {
        if self.journal == Some(settings) {
            return;
        }
        if !settings.write {
            self.flights.set_writing(false);
            self.writer = None;
        } else if self.writer.is_none() {
            let Some(writer) = Writer::start(rini_core::paths::journal_file()) else {
                return;
            };
            if let Some(burst) = self.flights.set_writing(true) {
                writer.send(burst);
            }
            self.writer = Some(writer);
        }
        self.journal = Some(settings);
    }

    fn carry_out(&mut self, command: Command, now: Instant) {
        match command {
            Command::Hold => {
                self.drawn.begun();
                self.pictures.hold(Hold::Transition)
            }
            Command::Settle => self.settle(),
            Command::Release => self.pictures.hold(Hold::Released),
            Command::AskFrameZero {
                generation,
                strip,
                background,
                behind,
            } => self.pictures.ask_on_screen(on_screen(
                self.tag(generation),
                Moment::FrameZero,
                &strip,
                &background,
                &behind,
            )),
            Command::Show => self.shown = self.render(Render::Show),
            Command::Write(frames) => self.owe(self.shown, Owed::Write(frames)),
            Command::Holes(ends) => {
                let holes = match (self.running.as_ref(), self.display) {
                    (Some(running), Some((_, scale))) => running.holes(ends, scale),
                    _ => ends.into_iter().map(|(_, end)| (end, 0.0)).collect(),
                };
                self.render(Render::Holes(holes));
            }
            Command::Watch(end) => {
                self.ends = end.compared.clone();
                self.watches.watch(&self.pictures, self.client, &end)
            }
            Command::StopWatches => self.watches.stop(&self.pictures),
            Command::StartMoving => self.start_moving(now),
            Command::Retarget { .. } => {
                let held = self
                    .running
                    .as_mut()
                    .map(|running| std::mem::take(&mut running.held))
                    .unwrap_or_default();
                for pass in held {
                    self.merge_now(pass);
                }
            }
            Command::AskFinal { generation, windows, behind } => self.pictures.ask_on_screen(
                on_screen(self.tag(generation), Moment::Final, &windows, &[], &behind),
            ),
            Command::Swap { generation, window } => self.swap(generation, window, now),
            Command::Snap { window, frame } => self.snap(window, frame, now),
            Command::KeepFocusedLook { generation, window } => {
                self.pictures.keep_focused(self.tag(generation), window)
            }
            Command::Focus { window, focused } => {
                if let Some(running) = self.running.as_mut() {
                    running.draw_focused(window, focused);
                }
                self.render(Render::Focus { window, focused });
            }
            Command::PlaceRest => {
                self.render(Render::PlaceRest { through: self.took });
            }
            Command::Lift { generation } => self.lift(generation, now),
            Command::Warm(windows) => {
                if !windows.is_empty() {
                    self.pictures.ask(Ask {
                        priority: Priority::Warm,
                        targets: sized_targets(windows),
                    });
                }
            }
            Command::Refresh(windows) => self.pictures.refresh(sized_targets(windows)),
            Command::ReportUp => self.report(true),
            Command::ReportSettled => self.report(false),
        }
    }

    fn tag(&self, generation: Generation) -> Tag {
        Tag {
            client: self.client,
            generation: generation.0,
        }
    }

    fn report(&self, flying: bool) { (self.on_flight)(flying) }

    /// `command` into the event's batch, numbered as its first command goes in: that batch's number.
    fn render(&mut self, command: Render) -> u64 {
        let Self { batch, drawn, .. } = self;
        let (seq, commands) = batch.get_or_insert_with(|| (drawn.batch(), Vec::new()));
        commands.push(command);
        *seq
    }

    /// The score's `edits` played, every tile of the flight handed with them.
    fn play(&mut self, edits: Vec<Edit>) {
        let Some(running) = self.running.as_ref() else { return };
        if edits.is_empty() {
            return;
        }
        let tiles = running.tiles.clone();
        self.render(Render::Play { edits, tiles });
    }

    /// `owed` once batch `after` is drawn: now, if it is.
    fn owe(&mut self, after: u64, owed: Owed) {
        if let Some(owed) = self.drawn.owe(after, owed) {
            self.carry(owed);
        }
    }

    fn carry(&mut self, owed: Owed) {
        match owed {
            Owed::Write(frames) => self.request_frames(frames),
            Owed::Settle => self.pictures.hold(Hold::Settling),
            Owed::Quiet(generation) => self.quiet_after(generation),
        }
    }

    /// The bar pictured again once the lift is drawn, or once a lift held hides and that is drawn.
    fn settle(&mut self) {
        match self.held.as_mut() {
            Some(held) => held.owed.push(Owed::Settle),
            None => self.owe(self.lifted, Owed::Settle),
        }
    }

    /// The event's batch to the renderer, if it drew anything, stamped as it is sent.
    fn send_batch(&mut self) {
        if let Some((seq, commands)) = self.batch.take() {
            (self.draw)(Batch {
                seq,
                sent: self.now(),
                commands,
            });
        }
    }

    /// The renderer's answer: a commit is journalled, then releases what waited for it. A drain with
    /// no overlay to draw on releases it too, as nothing will ever be drawn, and a flight running is
    /// abandoned, what it owes dropped, since its `Abandon` writes the final frames at once: a show
    /// sent since draws nothing either.
    fn rendered(&mut self, answer: crate::animation::event::Event) {
        match answer {
            crate::animation::event::Event::Committed {
                seq,
                overlay,
                shown,
                places,
                crops,
                sent,
                began,
                flushed,
            } => {
                self.flights
                    .drawn(self.now(), seq, (shown, places, crops), (sent, began, flushed));
                if overlay.is_some() {
                    self.overlay = overlay;
                }
                for owed in self.drawn.committed(seq) {
                    self.carry(owed);
                }
            }
            crate::animation::event::Event::Unavailable { seq } => {
                let flying = self.flights.coordinator().is_flying();
                for owed in self.drawn.undrawn(seq, flying) {
                    self.carry(owed);
                }
                if flying {
                    self.shown = self.shown.min(seq);
                    self.abandon();
                }
            }
        }
    }

    /// What the pictures service landed: the deliveries, the lost landings and the lost turns under
    /// this engine's tag go to the coordinator, each delivery journalled as it came and frame zero's
    /// onto their tiles first. The overlay's picture for a handover check is what a lift waiting for it hears, and the
    /// check's measure is journalled for a lift that hid on that picture. Any other capture is the
    /// cache's alone.
    fn pictures_landed(&mut self, landed: crate::pictures::event::Event) {
        match landed {
            crate::pictures::event::Event::Taken { .. } => {}
            crate::pictures::event::Event::Delivered { tag, why, sent, pictures } => {
                let Some(generation) = Generation::for_client(tag, self.client) else {
                    return;
                };
                let taken = pictures
                    .iter()
                    .map(|(window, picture, frame)| (*window, *frame, picture.taken));
                self.flights.delivery(self.now(), generation, why, sent, taken);
                if why == Why::FrameZero {
                    self.frame_zero(generation, pictures);
                    return;
                }
                self.offer(generation, why, pictures);
            }
            crate::pictures::event::Event::Lost { tag, window } => {
                let Some(generation) = Generation::for_client(tag, self.client) else {
                    return;
                };
                debug!(
                    pid = window.pid,
                    idx = window.idx.get(),
                    generation = generation.0,
                    "end window's picture will not come"
                );
                self.drive(lifecycle::Event::Lost { generation, window });
            }
            crate::pictures::event::Event::OverlayTaken { tag, taken } => {
                let Some(generation) = Generation::for_client(tag, self.client) else {
                    return;
                };
                self.lift_heard(Heard::OverlayTaken { generation, taken });
            }
            crate::pictures::event::Event::Handover { tag, windows } => {
                let Some(generation) = Generation::for_client(tag, self.client) else {
                    return;
                };
                self.flights.handover(self.now(), generation, windows);
            }
        }
    }

    /// Each delivered picture to the coordinator, which says whether it is swapped in now, held to be
    /// later, or never.
    fn offer(
        &mut self,
        generation: Generation,
        why: Why,
        pictures: Vec<(WindowId, Picture, ScreenRect)>,
    ) {
        for (window, picture, frame) in pictures {
            self.delivered.insert(window, (generation, frame, picture));
            self.drive(lifecycle::Event::Picture { generation, window, frame, why });
        }
    }

    /// Only the deliveries the coordinator still holds: once an event is carried out, one it does not
    /// will never be swapped in.
    fn keep_held(&mut self) {
        let coordinator = self.flights.coordinator();
        self.delivered.retain(|window, (generation, frame, _)| {
            coordinator.holds(*window, *generation, *frame)
        });
    }

    /// Frame zero's pictures, put on their tiles while the overlay is not up yet, then the overlay can
    /// go up; once it is up without them, each is offered to its tile as a swap, and frame zero is
    /// never installed again under a moving flight.
    fn frame_zero(
        &mut self,
        generation: Generation,
        pictures: Vec<(WindowId, Picture, ScreenRect)>,
    ) {
        if self.flights.coordinator().is_shown() {
            self.offer(generation, Why::FrameZero, pictures);
        } else if self.flights.coordinator().generation() == generation
            && self.flights.coordinator().is_flying()
            && let Some(running) = self.running.as_mut()
        {
            let worn = running.wear(
                pictures
                    .into_iter()
                    .map(|(window, picture, _)| {
                        let clipped = picture.source.clips();
                        (window, picture, clipped)
                    })
                    .collect(),
            );
            let worn = running.worn(&worn);
            if !worn.is_empty() {
                self.journal_wears(Wearing::FrameZero, worn);
                self.reinstall();
            }
        }
        self.drive(lifecycle::Event::FrameZero(generation));
    }

    /// The picture delivered of `window` under `generation`, onto its tile; a reserved entrance's
    /// first joins the flight with it, at the radius the window server reports now that it has drawn.
    /// One held for the corner of its tile's leg in two steps poses nothing before the motion, and
    /// once the leg plays has it installed again, carrying the picture from the corner.
    fn swap(&mut self, generation: Generation, window: WindowId, now: Instant) {
        if self
            .delivered
            .get(&window)
            .is_none_or(|(delivered, _, _)| *delivered != generation)
        {
            return;
        }
        let Some((_, _, picture)) = self.delivered.remove(&window) else {
            return;
        };
        let server_id = WindowServerId::from(window);
        let clipped = picture.source.clips();
        let at = self.media.at(now);
        let Some(running) = self.running.as_mut() else { return };
        let radius = || self.window_server.corner_radius(server_id);
        let took = running.take_picture(window, &picture, clipped, radius, at);
        let flown = running.flown;
        let worn = vec![(window, picture.source, picture.carries_blur, picture.taken)];
        match took {
            Took::Nothing => return,
            Took::Entrance(tile) => {
                self.journal_wears(Wearing::Swap, worn);
                return self.admit_entrance(*tile, now);
            }
            Took::Corner => {
                let again = running.score.again(window);
                self.journal_wears(Wearing::Corner, worn);
                if flown {
                    self.play(again);
                }
                return;
            }
            Took::Tile => self.journal_wears(Wearing::Swap, worn),
        }
        debug!(
            pid = window.pid,
            idx = window.idx.get(),
            generation = generation.0,
            flown,
            "picture swapped mid-flight"
        );
        if !flown {
            self.reinstall();
            return;
        }
        self.render(Render::Swap { window, picture, clipped });
    }

    /// The coordinator's snap of `window`'s tile onto `frame`, its window's, as an edit of the score
    /// played on the renderer.
    fn snap(&mut self, window: WindowId, frame: OverlayRect, now: Instant) {
        let at = self.media.at(now);
        let Some(running) = self.running.as_mut() else { return };
        let edits = running.score.snap(window, frame, at);
        self.play(edits);
    }

    /// A reserved entrance's tile, with its first picture: it joins frame zero, or the flight for what
    /// is left of it, merged in as a pass holding it alone.
    fn admit_entrance(&mut self, tile: OverlayTile, now: Instant) {
        let left = self.flights.coordinator().remaining(now).unwrap_or_default();
        let at = self.media.at(now);
        let viewport = self.viewport();
        let Some(running) = self.running.as_mut() else { return };
        debug!(
            pid = tile.window.pid,
            idx = tile.window.idx.get(),
            flown = running.flown,
            "entrance admitted"
        );
        if !running.flown {
            let loose = running.plan.position_of(plan::GroupKey::Loose);
            let (from, to) = (
                plan::group_relative(tile.from, loose),
                plan::group_relative(tile.to, loose),
            );
            running.plan.entrances.push((tile.window, from, to));
            self.reinstall();
            return;
        }
        let mut entrance = plan::ReflowPlan::empty();
        entrance.entrances.push((
            tile.window,
            plan::installed(tile.from),
            plan::installed(tile.to),
        ));
        let drawn = running.drawn();
        let pass = Pass {
            plan: &entrance,
            pan: None,
            focus: running.focus,
            viewport,
            drawn: &drawn,
            seconds: left.as_secs_f64(),
            cue: Cue::Plain,
            accent: None,
        };
        let merged = running.score.merge(&running.plan, &pass, at);
        running.plan = merged.plan;
        let banding = band_plan(&running.plan, &running.tiles, running.focus);
        let mut edits = merged.edits;
        edits.push(running.score.bank(banding));
        self.play(edits);
    }

    /// The display the overlay covers: what a member's destination is judged against.
    fn viewport(&self) -> OverlayRect {
        self.display
            .map(|(display, _)| to_overlay_space(display, display))
            .unwrap_or(OverlayRect::new(Point::new(-1e9, -1e9), Size::new(2e9, 2e9)))
    }

    /// The overlay's geometry. The pictures of the display are the service's, told by the reactor. A
    /// lift held for the overlay's picture hides first when the overlay is to move.
    fn set_display(&mut self, id: u32, frame: ScreenRect, scale: f64, top_band: f64) {
        let held = self.display.map(|(held, held_scale)| (held, self.top_band, held_scale));
        if held.is_some_and(|held| !same_layout(held, (frame, top_band, scale))) {
            self.lift_heard(Heard::Moved);
        }
        self.display = Some((frame, scale));
        self.display_id = Some(id);
        self.top_band = top_band;
        self.render(Render::Display { frame, top_band, scale });
    }

    /// The display the overlay is on, as the pictures of it are keyed.
    fn picture_display(&self) -> Option<Display> {
        let ((frame, scale), id) = (self.display?, self.display_id?);
        Some(Display { id, frame, scale })
    }

    /// Frame zero posed again, its plan as it is, with the pictures the tiles hold now; the accents
    /// waiting for the motion keep waiting.
    fn reinstall(&mut self) {
        let Some(running) = self.running.as_mut() else { return };
        let banding = band_plan(&running.plan, &running.tiles, running.focus);
        let drawn = running.drawn();
        let stage = Stage {
            plan: &running.plan,
            drawn: &drawn,
            seconds: 0.0,
            cue: running.cue,
            accents: &[],
        };
        let score = running.score.pose(&stage, banding).clone();
        let tiles = running.tiles.clone();
        self.render(Render::Stage { score, tiles });
    }

    /// Opens a transition from what a request alone says, before anything is composed, so its
    /// frame-zero pictures are asked for first. False when it could not run, and was abandoned.
    fn begin(
        &mut self,
        tiles: Vec<TileSummary>,
        final_frames: Vec<(WindowId, ScreenRect)>,
        (focus, focused_before): (Option<WindowId>, Option<WindowId>),
        key: Option<WindowId>,
        shape: Shape,
    ) -> bool {
        let Some((display, _)) = self.display else { return false };
        let stage = summary(
            tiles,
            final_frames,
            focus,
            losing_focus(focus, focused_before),
            None,
            key,
            shape,
            (display, self.rests),
            &|window| self.pictures.knowledge(WindowServerId::from(window)).is_opaque(),
        );
        self.quiet = None;
        let tx = self.tx.clone();
        self.frame_clock = RepeatingTimer::every(FRAME_INTERVAL, move || {
            tx.send(Event::Tick);
        });
        self.drive(lifecycle::Event::Begin(stage));
        if self.frame_clock.is_none() {
            warn!("could not start the frame clock; placing the windows directly");
            self.abandon();
            return false;
        }
        true
    }

    /// The transition could not be staged or shown: its windows are placed directly and nothing
    /// holds the pictures service.
    fn abandon(&mut self) {
        self.hide(Vec::new());
        self.running = None;
        self.frame_clock = None;
        self.ends.clear();
        self.drive(lifecycle::Event::Abandon);
    }

    /// What the flight composed, told to the coordinator.
    fn stage(&self, shape: Shape) -> Option<StageSummary> {
        let (running, (display, _)) = (self.running.as_ref()?, self.display?);
        Some(running.stage(shape, (display, self.rests), &|window| {
            self.pictures.knowledge(WindowServerId::from(window)).is_opaque()
        }))
    }

    /// The tile of the flight running that a later pass ending `window` `to` big lands it on, none
    /// with no flight or for a window joining: what that pass's window is judged against
    /// (`Landing`), the picture its tile carries into the leg bent now (`carried_into`). See "Mid-flight passes" in `src/transitions/docs/coordination.md`.
    fn landing(&self, window: WindowId, to: ScreenSize) -> Option<Landing> {
        let running = self.running.as_ref()?;
        let held = running
            .held
            .iter()
            .any(|held| held.shape.waits.grows().any(|grown| grown == window));
        let carried = running.carried_into(window, to, self.media.at(self.now()))?;
        Some(Landing { carried, to, held })
    }

    /// The focus a pass's tiles are drawn with, its own else the flight's, and the window they draw
    /// focused until it hands over: the flight's, else the one focused as the pass began.
    fn drawing_focus(
        &self,
        (focus, focused_before): (Option<WindowId>, Option<WindowId>),
    ) -> (Option<WindowId>, Option<WindowId>) {
        let focus = focus.or(self.running.as_ref().and_then(|running| running.focus));
        let shown = self.running.as_ref().map_or(focused_before, |running| running.shown_focus);
        (focus, losing_focus(focus, shown))
    }

    fn start(
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
        let Some((display_frame, _)) = self.display else {
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
        if !self.flights.coordinator().is_flying() {
            let requested = windows
                .iter()
                .map(|request| TileSummary {
                    window: request.window,
                    start: request.from,
                    end: request.to,
                    floating: request.floating,
                })
                .collect();
            if !self.begin(
                requested,
                final_frames.clone(),
                (focus, focused_before),
                key,
                shape.clone(),
            ) {
                return;
            }
        }
        let (drawing_focus, losing) = self.drawing_focus((focus, focused_before));

        let ws = &*self.window_server;
        let stacking = Stacking::read(ws, &windows.iter().map(|r| r.server_id).collect::<Vec<_>>());

        let mut tiles = Vec::with_capacity(windows.len());
        let mut skipped = 0usize;
        let mut offscreen = 0usize;
        let mut needs_capture: Vec<PictureTarget> = Vec::new();
        let mut entrances: Vec<PendingEntrance> = Vec::new();
        let mut waits = Waits::default();
        // Newly opened windows travelling from their spawn frame.
        let mut spawn_entrances: Vec<(WindowId, GroupRect, GroupRect)> = Vec::new();
        let mut sync_captures = 0usize;
        let scale = self.display.map(|(_, scale)| scale).unwrap_or(2.0);
        let mut resolved: Vec<(WindowId, ScreenRect, ScreenRect, bool)> = Vec::new();
        for (index, request) in windows.iter().enumerate() {
            let live = live_frame(ws, request.server_id);
            let path = tile_path(live, &windows, index, display_frame);
            let Some(TilePath { start, end }) = path else {
                offscreen += 1;
                debug!(
                    wsid = request.server_id.as_u32(),
                    from = %Whole(request.from),
                    to = %Whole(request.to),
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
            // leaving a hole; see `stands_in`. The flight holds for it, and its picture replaces the
            // stand-in once it has repainted where it ends.
            let snapshot = picture_or_stand_in(
                snapshot,
                || ws.get_window(request.server_id).map(|info| ScreenRect::from_cg(info.frame)),
                display_frame,
                || self.pictures.stand_in(request.to.size),
            );
            let flies = match snapshot {
                Some(snapshot) => {
                    let flies = FliesOn::Picture {
                        source: snapshot.source,
                        covered: snapshot.coverage.covered,
                    };
                    let clipped = snapshot.source.clips();
                    let focused = drawn_focused(request.window, drawing_focus, losing);
                    let tile = stacking.tile_of(
                        request.window,
                        request.server_id,
                        request.floating,
                        request.worn,
                    );
                    let path = (
                        to_overlay_space(start, display_frame),
                        to_overlay_space(end, display_frame),
                    );
                    tiles.push(tile.flying(path, snapshot, clipped, focused));
                    resolved.push((request.window, start, end, request.floating));
                    flies
                }
                // No picture: almost always a window that just opened. See "A window that opens"
                // in `src/transitions/docs/coordination.md`.
                None => {
                    // Only a frame on this display counts as a spawn; capturing off screen is slow.
                    let spawn = ws
                        .get_window(request.server_id)
                        .map(|info| ScreenRect::from_cg(info.frame))
                        .filter(|f| !rini_geometry::is_off_screen(display_frame, *f));
                    let budget_left = sync_captures < MAX_SYNC_ENTRANCE_CAPTURES;
                    let captured_at = Instant::now();
                    let picture = spawn
                        .filter(|f| f.size.width > 0.0 && f.size.height > 0.0 && budget_left)
                        .and_then(|_| {
                            sync_captures += 1;
                            (self.capture)(ws, request.server_id, scale)
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
                            let from_o = to_overlay_space(from, display_frame);
                            let to_o = to_overlay_space(to, display_frame);
                            // Already at its slot (a cold cache, not an open): a still tile.
                            let standing = from.same_as(to);
                            let focused =
                                !standing || drawn_focused(request.window, drawing_focus, losing);
                            let mut tile = stacking.tile_of(
                                request.window,
                                request.server_id,
                                request.floating,
                                request.worn,
                            );
                            if !standing {
                                tile.server_order = Some(0);
                            }
                            tiles.push(tile.flying((from_o, to_o), snapshot, clipped, focused));
                            if standing {
                                resolved.push((request.window, from, to, request.floating));
                            } else {
                                let entrance = (plan::installed(from_o), plan::installed(to_o));
                                spawn_entrances.push((request.window, entrance.0, entrance.1));
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
                            entrances.push(PendingEntrance {
                                window: request.window,
                                to: to_overlay_space(request.to, display_frame),
                                floating: request.floating,
                                worn: request.worn,
                            });
                        }
                    }
                    FliesOn::Entrance(decided)
                }
            };
            let landing = flies.ends_at(end.size).and_then(|to| self.landing(request.window, to));
            waits.push_request(request, flies, landing, start, display_frame);
        }
        restack(&mut tiles, drawing_focus);
        self.ask_missing(needs_capture);
        debug!(
            requested = windows.len(),
            tiles = tiles.len(),
            offscreen,
            no_snapshot = skipped,
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

    /// Runs one composed pass: merged into the flight running, or posed as its frame zero.
    /// See "The overlay engine" in `src/animation/docs/animation-smoothness.md`.
    fn begin_group(&mut self, pass: Composed) {
        if self.running.is_some() {
            self.merge_group(pass);
            return;
        }
        // Guards the strip path: a pan with no usable picture leaves nothing to draw.
        if pass.tiles.is_empty() {
            self.abandon();
            return;
        }
        if let (Some(at), Some((display, _))) = (self.picture_display(), self.display) {
            let dressing = self.pictures.dressing(&*self.window_server, at);
            let strip = dressing.strip.map(|strip| to_overlay_space(strip, display));
            log_dressing(pass.label, &dressing, strip);
            self.render(Render::Dress {
                desktop: dressing.desktop,
                bar: dressing.bar,
                strip,
            });
        }
        let (focus, losing) = self.drawing_focus(pass.focus);
        let score = FlightScore::new(pass.look);
        let mut running = RunningAnimation {
            tiles: pass.tiles,
            final_frames: pass.final_frames,
            entrances: pass.entrances,
            focus,
            shown_focus: losing.or(focus),
            key: pass.key,
            focused_look: None,
            plan: score.plan(pass.plan, &pass.cue),
            flown: false,
            score,
            cue: pass.cue,
            held: Vec::new(),
        };
        if let Some(accent) = pass.accent {
            running.score.wait(accent);
        }
        restack(&mut running.tiles, focus);
        let composed = running.worn(&running.drawn());
        self.running = Some(running);
        self.journal_wears(Wearing::Compose, composed);
        let Some(mut stage) = self.stage(pass.shape) else {
            return;
        };
        if let Some(target) = stage.parked_focus()
            && self.fly_focused_look(target)
        {
            stage.focused_look = Some(target);
        }
        self.reinstall();
        self.drive(lifecycle::Event::Merge { stage, moved: false });
    }

    /// A focus target parked off the display flies its focused look, if the service holds one that
    /// fits: frame zero cannot picture it, and it arrives focused. Whether it does.
    fn fly_focused_look(&mut self, target: WindowId) -> bool {
        let look = self.pictures.focused(target);
        let clipped = look.as_ref().is_some_and(|look| look.source.clips());
        let worn = look.as_ref().map(|look| (target, look.source, look.carries_blur, look.taken));
        let flown = self
            .running
            .as_mut()
            .is_some_and(|running| running.fly_focused_look(target, look, clipped));
        if flown {
            self.journal_wears(Wearing::FocusedLook, worn.into_iter().collect());
        }
        flown
    }

    /// The journal's record of each picture a tile took `at` that moment of the flight: its route,
    /// whether it carries its window's blur, and when it was taken.
    fn journal_wears(&mut self, at: Wearing, worn: Vec<(WindowId, Route, bool, Instant)>) {
        let now = self.now();
        for (window, route, blur, taken) in worn {
            self.flights.wears(now, window, at, route, blur, taken);
        }
    }

    /// On a flight that moves, the tiles a merge turned focused or not drawn so, the focused look a
    /// new parked target flies cut onto its tile, and the ring and border of every tile whose end the
    /// merge changed worn at once, as the overlay showed the first pass's: a retarget draws none.
    fn redraw_focus(&mut self, before: &[(WindowId, (bool, Worn))], look: Option<WindowId>) {
        let Some(running) = self.running.as_ref() else { return };
        let mut drawn = Vec::new();
        for tile in running.tiles.iter() {
            let was = before.iter().find(|(w, _)| *w == tile.window).map(|(_, drawn)| *drawn);
            let (window, focused, worn) = (tile.window, tile.focused, tile.worn);
            if look == Some(window) {
                drawn.push(Render::Swap {
                    window,
                    picture: tile.snapshot.clone(),
                    clipped: tile.clipped,
                });
                drawn.push(Render::Focus { window, focused: true });
            } else if was.is_some_and(|(was, _)| was != focused) {
                drawn.push(Render::Focus { window, focused });
            }
            if was.is_some_and(|(_, was)| was != worn) {
                drawn.push(Render::Wear { window, worn });
            }
        }
        for command in drawn {
            self.render(command);
        }
    }

    /// One later pass reaching the flight running, its windows sorted against the pictures their
    /// tiles carry and the grows of the passes held as it was composed (`FlightEngine::landing`):
    /// once the plan plays, one growing a window, and any while one is held, waits for the
    /// coordinator to say when it merges (`Event::HoldMerge`); any other merges now. See "Mid-flight
    /// passes" in `src/transitions/docs/coordination.md`.
    fn merge_group(&mut self, pass: Composed) {
        let Some(running) = self.running.as_mut() else { return };
        let grows: Vec<(WindowId, OverlayRect)> = pass
            .shape
            .waits
            .grows()
            .filter_map(|window| {
                pass.tiles
                    .iter()
                    .find(|tile| tile.window == window)
                    .map(|tile| (window, tile.to))
            })
            .collect();
        if !running.flown || (grows.is_empty() && running.held.is_empty()) {
            return self.merge_now(pass);
        }
        let writes = pass.final_frames.clone();
        running.held.push(pass);
        let Some((display, _)) = self.display else { return };
        let grows = grows
            .into_iter()
            .map(|(window, to)| (window, from_overlay_space(to, display)))
            .collect();
        self.drive(lifecycle::Event::HoldMerge { grows, writes });
    }

    /// One later pass folded into the flight running: bent by its look from the score, from where
    /// each tile is drawn now, once the plan plays, folded into frame zero and posed again before. A
    /// pass moving focus to a target parked off the display flies its focused look, as the first
    /// pass's does.
    fn merge_now(&mut self, pass: Composed) {
        let now = self.now();
        let at = self.media.at(now);
        let left = self.flights.coordinator().remaining(now).unwrap_or_default();
        let viewport = self.viewport();
        let display = self.display.map(|(frame, _)| frame);
        let Some(running) = self.running.as_mut() else { return };
        for entrance in pass.entrances {
            if !running.entrances.iter().any(|e| e.window == entrance.window) {
                running.entrances.push(entrance);
            }
        }
        if let Some(display) = display {
            retarget_entrances(&mut running.entrances, &pass.final_frames, display);
        }
        extend_latest(&mut running.final_frames, pass.final_frames.into_iter());
        let focus_before = running.focus;
        let drawn_focused_before: Vec<(WindowId, (bool, Worn))> =
            running.tiles.iter().map(|t| (t.window, (t.focused, t.worn))).collect();
        let (pass_focus, _) = pass.focus;
        let joining: Vec<WindowId> = pass
            .tiles
            .iter()
            .map(|tile| tile.window)
            .filter(|window| !running.drawn().contains(window))
            .collect();
        running.merge_pass(pass.tiles, pass_focus);
        let composed = running.worn(&joining);
        let mut shape = pass.shape;
        let drawn = running.drawn();
        let folded = Pass {
            plan: &pass.plan,
            pan: pass.pan,
            focus: running.focus,
            viewport,
            drawn: &drawn,
            seconds: shape.duration.as_secs_f64(),
            cue: pass.cue,
            accent: pass.accent,
        };
        let mut moved = false;
        let mut played = Vec::new();
        // See "Mid-flight passes" in `src/transitions/docs/coordination.md`.
        if let Some((mut delta, mut edits)) = running.take_pass(&folded, at) {
            delta.focus_changed = pass_focus.is_some() && pass_focus != focus_before;
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
                    let drawn = match running.score.score().sample(Node::Container(*key), at) {
                        Some(Value::Point(drawn)) => drawn,
                        _ => OverlayPoint::origin(),
                    };
                    debug!(
                        key = format!("{key:?}"),
                        presented = %Whole(drawn),
                        to = %Whole(*to),
                        "group retargeted"
                    );
                }
                for (window, from_key, to_key) in &delta.reparented {
                    let dest = running.plan.destination(*window).map(Whole);
                    let from_to = running.plan.position_of(*from_key);
                    debug!(
                        pid = window.pid,
                        idx = window.idx.get(),
                        from = format!("{from_key:?}"),
                        to = format!("{to_key:?}"),
                        group_destination = %Whole(from_to),
                        member_destination = dest.map(|dest| dest.to_string()).unwrap_or_default(),
                        "member reparented"
                    );
                }
            }
            moved = delta.moves_anything();
            let banding = band_plan(&running.plan, &running.tiles, running.focus);
            edits.push(running.score.bank(banding));
            played = edits;
            // A merge that moves anything restarts the clock, over everything the score plays.
            shape.duration = running.score.clock(shape.duration, at);
        }
        let flown = running.flown;
        let extend = (flown && !moved).then(|| running.score.outlasting(left, at)).flatten();
        let focus_moved = running.focus != focus_before;
        self.journal_wears(Wearing::Compose, composed);
        self.play(played);
        let look = if focus_moved
            && let Some(target) = self.stage(shape.clone()).and_then(|stage| stage.parked_focus())
            && self.fly_focused_look(target)
        {
            Some(target)
        } else {
            None
        };
        if flown {
            self.redraw_focus(&drawn_focused_before, look);
        } else {
            self.reinstall();
        }
        let Some(stage) = self.stage(shape) else { return };
        self.drive(lifecycle::Event::Merge { stage, moved });
        if let Some(extend) = extend {
            self.drive(lifecycle::Event::Extend(extend));
        }
    }

    /// Animates the whole strip surface as one rigid group, one container and one position
    /// animation, with a container of its own for each column a move swapped that crosses it, and a
    /// move's step (`accent`) stepping every strip container. A bounce from rest is one with no
    /// travel. See "The move flight, staged" in `src/transitions/docs/coordination.md`.
    #[allow(clippy::too_many_arguments, reason = "no roadmap task groups them")]
    fn start_surface(
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
        if !self.flights.coordinator().is_flying() {
            let requested = windows
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
                .collect();
            if !self.begin(requested, final_frames.clone(), focus, key, shape.clone()) {
                return;
            }
        }
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
        for window in &windows {
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

    /// Pushes the strip surface out by `overshoot` and back, additively on a flight that moves, as
    /// the only motion of a no-travel one when none runs, and as the flight starts on one that does
    /// not move yet. Every push that reaches a moving flight shows: one arriving while a bounce plays
    /// starts each container's next from where its own is drawn. See "Edge bounce" in
    /// `src/transitions/docs/coordination.md`.
    fn start_bounce(
        &mut self,
        windows: Vec<SurfaceWindow>,
        overshoot: OverlayVector,
        final_frames: Vec<(WindowId, ScreenRect)>,
        focus: Option<WindowId>,
        duration: Duration,
        (look, cue): (Look, Cue),
    ) {
        if !self.flights.coordinator().is_flying() {
            let at_rest = SurfacePoint::origin();
            // A push against an end moves no focus.
            self.start_surface(
                windows,
                at_rest,
                at_rest,
                final_frames,
                (focus, None),
                None,
                duration,
                (look, cue, None),
                Some(Kind::Bounce),
            );
        }
        let at = self.media.at(self.now());
        let Some(running) = self.running.as_mut() else { return };
        let Some(edits) = running.push(&cue, overshoot, duration.as_secs_f64(), at) else {
            return;
        };
        let remaining = running.score.remaining(at);
        self.play(edits);
        debug!(
            overshoot = %Whole(overshoot),
            "edge bounce"
        );
        self.drive(lifecycle::Event::Extend(remaining));
    }

    /// The motion starts: the look composes it from frame zero, with the accents that waited for it,
    /// and the renderer plays it in one transaction; the clock runs on for as long as the score does.
    fn start_moving(&mut self, now: Instant) {
        let duration = self.flights.coordinator().remaining(now).unwrap_or_default();
        let at = self.media.at(now);
        let Some(running) = self.running.as_mut() else { return };
        running.flown = true;
        // The acceptance greps count this line against "overlay lifted".
        let flight = &running.plan;
        debug!(
            groups = flight.groups.iter().filter(|g| !g.members.is_empty()).count(),
            changing = flight.changing.len(),
            entrances = flight.entrances.len(),
            floating = flight.floating.len(),
            "flight composed"
        );
        let drawn = running.drawn();
        let stage = Stage {
            plan: &running.plan,
            drawn: &drawn,
            seconds: duration.as_secs_f64(),
            cue: running.cue,
            accents: &[],
        };
        let edits = running.score.start_moving(&stage, at);
        running.turn_corners(at);
        let remaining = running.score.remaining(at);
        self.play(edits);
        self.drive(lifecycle::Event::Extend(remaining));
    }

    /// One frame of the clock: where the tiles are drawn and where the windows are, read only from a
    /// tick before the clock is done, for the rest windows' place and the lift; the tiles are where
    /// they end only once every batch is committed, the place's among them. It draws
    /// only the trace of where each container is presented, and that only while its target is on.
    fn tick(&mut self) {
        if self.flights.coordinator().is_flying() {
            let now = self.now();
            let (settled, landed) = if self.flights.coordinator().wants_landing(now) {
                let at = self.media.at(now);
                let scored = self.running.as_ref().is_none_or(|running| running.score.settled(at));
                let landed = self.handover().as_ref().is_none_or(|report| report.count_over == 0);
                (scored && self.drawn.caught_up(), landed)
            } else {
                (false, false)
            };
            self.drive(lifecycle::Event::Tick { settled, landed });
        }
        if tracing::enabled!(target: TRACE_TARGET, tracing::Level::TRACE) {
            self.render(Render::Trace);
        }
    }

    /// Overlay down, over the real windows where they end; the quiet after it begins once that is
    /// drawn. While the handover is measured, the hide waits for the overlay's own picture, and the
    /// quiet with it.
    fn lift(&mut self, generation: Generation, now: Instant) {
        self.report_handover_error();
        // The acceptance greps pair "placing real windows" with this line.
        let windows = self.running.as_ref().map_or(0, |running| running.tiles.len());
        debug!(windows, "overlay lifted");
        match self.handover_check(generation) {
            Some(check) => self.hold_lift(generation, check, now),
            None => self.hide(vec![Owed::Quiet(generation)]),
        }
        self.running = None;
        self.frame_clock = None;
    }

    /// The quiet after `generation`'s lift, from now.
    fn quiet_after(&mut self, generation: Generation) {
        let tx = self.tx.clone();
        match RepeatingTimer::every(SETTLE_BEFORE_CAPTURES, move || {
            tx.send(Event::Quiet(generation));
        }) {
            Some(timer) => self.quiet = Some((generation, timer)),
            None => self.tx.send(Event::Quiet(generation)),
        }
    }

    /// What measuring `generation`'s lift compares: the overlay against the screen over the end windows
    /// the last watch named. `None` unless the lift is measured (`journal::measures`).
    fn handover_check(&mut self, generation: Generation) -> Option<HandoverCheck> {
        let windows = std::mem::take(&mut self.ends);
        if !measures(
            self.journal.is_some_and(|journal| journal.measure_handover),
            &windows,
        ) {
            return None;
        }
        let (display, scale) = self.display?;
        let band =
            |window| self.running.as_ref().map_or(0.0, |running| running.band(window, scale));
        let windows =
            windows.into_iter().map(|(window, end)| (window, end, band(window))).collect();
        Some(HandoverCheck {
            tag: self.tag(generation),
            overlay: self.overlay?,
            overlay_origin: window_origin(display, self.top_band),
            display,
            scale,
            windows,
        })
    }

    /// The pictures service measures the lift, and the hide waits for the overlay's own picture, up to
    /// `OVERLAY_TAKE_PATIENCE`, with the quiet and `Settle`; with no timer for the patience, nothing is
    /// measured and it does not wait.
    fn hold_lift(&mut self, generation: Generation, check: HandoverCheck, now: Instant) {
        let quiet = Owed::Quiet(generation);
        let tx = self.tx.clone();
        let Some(patience) =
            RepeatingTimer::every(OVERLAY_TAKE_PATIENCE, move || tx.send(Event::LiftPatience))
        else {
            self.hide(vec![quiet]);
            return;
        };
        self.pictures.check_handover(check);
        self.held = Some(HeldLift {
            _patience: patience,
            owed: vec![quiet],
        });
        self.flights.lifted(generation, now);
    }

    /// What a lift held for the overlay's picture hears: it hides once the journal says so.
    fn lift_heard(&mut self, heard: Heard) {
        if self.flights.heard(heard, self.now())
            && let Some(held) = self.held.take()
        {
            self.hide(held.owed);
        }
    }

    /// The overlay down, and its tiles let go; `owed` waits for that to be drawn.
    fn hide(&mut self, owed: Vec<Owed>) {
        self.lifted = self.render(Render::Lift { through: self.took });
        for owed in owed {
            self.owe(self.lifted, owed);
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
        let ids: Vec<WindowServerId> =
            asked.iter().map(|w| WindowServerId::new(w.idx.get())).collect();
        let by_server = frames_of(&*self.window_server, &ids);
        let real: HashMap<WindowId, ScreenRect> = asked
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

    /// Asks the pictures service for the windows a pass has no picture of, for the next flight: it
    /// holds the ask until the quiet, and frame zero pictures what is on screen for this one.
    fn ask_missing(&self, targets: Vec<PictureTarget>) {
        if targets.is_empty() {
            return;
        }
        debug!(
            count = targets.len(),
            "asking for pictures the flight has none of"
        );
        self.pictures.ask(Ask {
            priority: Priority::Warm,
            targets,
        });
    }

    /// Asks the reactor to place windows at their final frames.
    fn request_frames(&self, frames: Vec<(WindowId, ScreenRect)>) {
        if frames.is_empty() {
            return;
        }
        debug!(count = frames.len(), "placing real windows behind the overlay");
        (self.place_frames)(frames);
    }
}

/// The frame the window server has for each of `ids` it knows, in one query.
fn frames_of(ws: &dyn WindowServer, ids: &[WindowServerId]) -> HashMap<WindowServerId, ScreenRect> {
    let windows = ws.get_windows(ids).into_iter();
    windows.map(|info| (info.id, ScreenRect::from_cg(info.frame))).collect()
}

/// The frame the window server reports for a window, if it reports a real one: the reactor's
/// `from` can be the previous pass's destination rather than where the window sits.
///
/// A zero-sized frame is macOS saying it has not laid the window out yet, which is not a position to
/// animate from. The geometry decision is `travel::tile_path`; this is the read it needs.
fn live_frame(ws: &dyn WindowServer, server_id: WindowServerId) -> Option<ScreenRect> {
    ws.get_window(server_id)
        .map(|info| ScreenRect::from_cg(info.frame))
        .filter(|frame| frame.size.width > 0.0 && frame.size.height > 0.0)
}

/// Records what the overlay was dressed with; a black backdrop is only diagnosable after the fact.
fn log_dressing(
    path: &str,
    dressing: &crate::pictures::platform::service::Dressing,
    strip: Option<OverlayRect>,
) {
    debug!(
        path,
        backdrop = dressing
            .desktop
            .as_ref()
            .map(|b| format!("{} {:?}", Whole(b.coverage.covered), b.source))
            .unwrap_or_else(|| "NONE".to_string()),
        bar = dressing
            .bar
            .as_ref()
            .map(|b| Whole(b.coverage.covered).to_string())
            .unwrap_or_else(|| "NONE".to_string()),
        strip = strip
            .map(|strip| Whole(strip).to_string())
            .unwrap_or_else(|| "none".to_string()),
        "overlay dressed"
    );
}

#[cfg(test)]
mod tests {
    use rini_geometry::space::{Respace, ScreenSize, ScreenVector, SurfaceRect, ToCg};

    use super::*;
    use crate::animation::domain::motion::surface::viewport;
    use crate::animation::domain::motion::testing::{DISPLAY, Gen, RUNS, VIEWPORT, rect};
    use crate::decorations::domain::style::Ring;
    use crate::pictures::platform::picture::test_picture;

    /// A frame on `DISPLAY`, which sits at the origin, on the overlay over it.
    fn on(frame: ScreenRect) -> OverlayRect { to_overlay_space(frame, DISPLAY) }

    /// An overlay frame back on `DISPLAY`.
    fn screen(frame: OverlayRect) -> ScreenRect { from_overlay_space(frame, DISPLAY) }

    /// A frame on the strip surface where the viewport at rest shows it.
    fn seen(frame: SurfaceRect) -> OverlayRect {
        viewport(SurfacePoint::origin()).transform_rect(&frame)
    }

    fn wid(idx: u32) -> WindowId { WindowId::new(7, idx) }

    /// A flight with nothing in it, composed and not yet moving.
    fn flight() -> RunningAnimation {
        RunningAnimation {
            tiles: Vec::new(),
            final_frames: Vec::new(),
            entrances: Vec::new(),
            focus: None,
            shown_focus: None,
            key: None,
            focused_look: None,
            plan: plan::FlightPlan::empty(),
            flown: false,
            score: FlightScore::new(Look::default()),
            cue: Cue::Plain,
            held: Vec::new(),
        }
    }

    /// A tile drawn from a picture the size of its destination.
    fn stacked(
        window: WindowId,
        from: OverlayRect,
        to: OverlayRect,
        server_order: Option<usize>,
        floating: bool,
    ) -> OverlayTile {
        let tile = tiles::TileOf {
            window,
            floating,
            server_order,
            worn: Worn::default(),
            radius: None,
        };
        tile.flying((from, to), test_picture(to.size), false, false)
    }

    /// A SkyLight sliver: 40pt of an `size`-wide window, which `is_usable` rejects.
    fn clipped(size: ScreenSize) -> Picture {
        let mut snapshot = test_picture(size);
        snapshot.coverage.covered = ScreenSize::new(40.0, size.height);
        snapshot
    }

    fn layout(waits: Waits) -> Shape {
        Shape {
            kind: Kind::LayoutPass,
            start: Start::Coalesced,
            duration: Duration::from_millis(300),
            waits,
        }
    }

    /// What the engine tells the coordinator and asks of the pictures service, carried out over
    /// plain values.
    mod translation {
        use super::*;
        use crate::pictures::platform::service::Command as Asked;

        fn asked(commands: &mut channel::Receiver<Asked>) -> Vec<Asked> {
            let mut out = Vec::new();
            while let Ok((_, command)) = commands.try_recv() {
                out.push(command);
            }
            out
        }

        fn end(generation: u64, now: Instant) -> EndWatch {
            EndWatch {
                generation: Generation(generation),
                compared: vec![
                    (wid(1), rect(4.0, 32.0, 859.0, 1081.0)),
                    (wid(2), rect(867.0, 32.0, 859.0, 1081.0)),
                ],
                landing: vec![(wid(1), rect(4.0, 32.0, 859.0, 1081.0))],
                awaiting: vec![(wid(2), rect(867.0, 32.0, 859.0, 1081.0))],
                landing_until: now + Duration::from_millis(150),
                awaiting_until: now + HOLD_CAP,
                focus: Some((wid(1), rect(4.0, 32.0, 859.0, 1081.0))),
                losing_focus: Some((wid(3), rect(867.0, 32.0, 859.0, 1081.0))),
                background: vec![(wid(9), rect(224.0, 95.0, 600.0, 400.0))],
                background_initial: Some(now + Duration::from_millis(30)),
                behind: vec![wid(9)],
            }
        }

        #[test]
        fn the_end_state_is_watched_one_slot_a_kind_and_retargeted_from_one_generation_to_the_next()
        {
            let (pictures, mut commands) = Pictures::detached();
            let client = Client(1);
            let now = Instant::now();
            let mut watches = EndWatches::default();
            watches.watch(&pictures, client, &end(3, now));
            let tag = Tag { client, generation: 3 };
            let first = asked(&mut commands);
            assert_eq!(first.len(), 4, "{first:?}");
            match &first[0] {
                Asked::Watch(
                    id,
                    Watch::Landing {
                        tag: under,
                        windows,
                        behind,
                        slack,
                        settle,
                        until,
                    },
                ) => {
                    assert_eq!(Some(*id), watches.landing);
                    assert_eq!(*under, tag);
                    let target = &windows[0].0;
                    assert_eq!(
                        (target.window, target.server_id),
                        (wid(1), WindowServerId::new(1))
                    );
                    assert_eq!(
                        *behind,
                        vec![WindowServerId::new(9)],
                        "the strip is captured over them"
                    );
                    assert_eq!(
                        (*slack, *settle, *until),
                        (
                            END_SLACK,
                            Settle::AsSoonAsThere,
                            now + Duration::from_millis(150)
                        )
                    );
                }
                other => panic!("{other:?}"),
            }
            match &first[1] {
                Asked::Watch(id, Watch::Landing { windows, settle, until, .. }) => {
                    assert_eq!(Some(*id), watches.awaiting);
                    assert_eq!(windows[0].0.window, wid(2));
                    assert_eq!(
                        (*settle, *until),
                        (Settle::Repainted, now + HOLD_CAP),
                        "watched as long as it is held for"
                    );
                }
                other => panic!("{other:?}"),
            }
            match &first[2] {
                Asked::Watch(
                    id,
                    Watch::Change {
                        window, losing, every, tries, ..
                    },
                ) => {
                    assert_eq!(Some(*id), watches.change);
                    assert_eq!(window.window, wid(1));
                    let (losing, at) = losing.expect("the window focus leaves, beside it");
                    assert_eq!(
                        (losing.window, losing.size, at),
                        (wid(3), at.size, rect(867.0, 32.0, 859.0, 1081.0))
                    );
                    assert_eq!((*every, *tries), (CHANGE_EVERY, CHANGE_TRIES));
                }
                other => panic!("{other:?}"),
            }
            match &first[3] {
                Asked::Watch(
                    _,
                    Watch::Live {
                        windows, every, after, initial, ..
                    },
                ) => {
                    assert_eq!(windows[0].window, wid(9));
                    assert_eq!(*every, LIVE_EVERY);
                    assert_eq!(
                        *after, watches.change,
                        "kept fresh once the focus target's look is in"
                    );
                    assert_eq!(*initial, Some(now + Duration::from_millis(30)));
                }
                other => panic!("{other:?}"),
            }

            // The next generation: everything has landed but what repaints, and the focus is in.
            let mut next = end(4, now);
            next.landing.clear();
            next.focus = None;
            next.losing_focus = None;
            let (awaiting, live) = (watches.awaiting, watches.live);
            watches.watch(&pictures, client, &next);
            let second = asked(&mut commands);
            assert!(
                matches!(&second[..], [
                    Asked::Stop(_),
                    Asked::Retarget(_, Watch::Landing { settle: Settle::Repainted, .. }),
                    Asked::Stop(_),
                    Asked::Retarget(_, Watch::Live { after: None, .. }),
                ]),
                "{second:?}"
            );
            assert_eq!(
                (watches.awaiting, watches.live),
                (awaiting, live),
                "retargeted, not watched afresh"
            );
            assert_eq!((watches.landing, watches.change), (None, None));

            watches.stop(&pictures);
            assert!(matches!(&asked(&mut commands)[..], [
                Asked::Stop(_),
                Asked::Stop(_)
            ]));
            assert_eq!(watches, EndWatches::default());
        }

        #[test]
        fn an_ask_of_windows_on_screen_carries_the_engines_tag_and_each_window_where_it_is_expected()
         {
            let client = Client(1);
            let tag = Tag { client, generation: 5 };
            let at: ScreenRect = rect(4.0, 32.0, 859.0, 1081.0);
            let floating = rect(224.0, 95.0, 600.0, 400.0);
            let ask = on_screen(
                tag,
                Moment::FrameZero,
                &[(wid(1), at)],
                &[(wid(9), floating)],
                &[wid(9)],
            );
            assert_eq!((ask.tag, ask.moment), (tag, Moment::FrameZero));
            let size = at.size;
            let target = PictureTarget {
                window: wid(1),
                server_id: WindowServerId::new(1),
                size,
            };
            assert_eq!(ask.strip, vec![(target, at)]);
            assert_eq!(ask.background.len(), 1);
            assert_eq!(ask.behind, vec![WindowServerId::new(9)]);
            assert_eq!(
                sized_targets(vec![(wid(1), at)]),
                vec![target],
                "warmed at the size it ended"
            );
        }

        #[test]
        fn the_coordinator_hears_every_tile_and_reserved_entrance_in_screen_points() {
            let display = rect(1728.0, 0.0, 1728.0, 1117.0);
            let slot = rect(4.0, 32.0, 859.0, 1081.0);
            let next = rect(867.0, 32.0, 859.0, 1081.0);
            let mut running = flight();
            running.tiles.push(stacked(wid(1), next, slot, Some(0), false));
            running.entrances.push(PendingEntrance {
                window: wid(2),
                to: next,
                floating: true,
                worn: Default::default(),
            });
            running.final_frames = vec![(wid(1), from_overlay_space(slot, display))];
            running.focus = Some(wid(2));
            running.shown_focus = Some(wid(1));
            let waits = [(wid(2), EndPicture::Awaited), (wid(1), EndPicture::Kept)]
                .into_iter()
                .collect();
            let stage = running.stage(layout(waits), (display, true), &|window| window == wid(1));
            assert_eq!(stage.tiles[0], TileSummary {
                window: wid(1),
                start: rect(2595.0, 32.0, 859.0, 1081.0),
                end: rect(1732.0, 32.0, 859.0, 1081.0),
                floating: false,
            });
            assert_eq!(
                stage.tiles[1],
                TileSummary {
                    window: wid(2),
                    start: from_overlay_space(entrance_from(next), display),
                    end: rect(2595.0, 32.0, 859.0, 1081.0),
                    floating: true,
                },
                "a reserved entrance, drawn once its picture is swapped in"
            );
            assert_eq!(stage.final_frames, running.final_frames);
            assert_eq!(
                stage.losing_focus,
                Some(wid(1)),
                "drawn focused until focus hands over"
            );
            assert_eq!(stage.opaque, vec![wid(1)]);
            assert_eq!(
                (stage.awaiting, stage.repainting, stage.kept),
                (vec![wid(2)], vec![wid(1)], vec![wid(1)])
            );
            assert_eq!(stage.display, display);
            running.shown_focus = Some(wid(2));
            assert_eq!(
                running
                    .stage(layout(Waits::default()), (display, true), &|_| false)
                    .losing_focus,
                None,
                "handed over"
            );
        }
    }

    /// What the flight composed, as the coordinator's commands change it.
    mod composed {
        use super::*;

        #[test]
        fn frame_zeros_pictures_are_worn_by_their_tiles_and_a_window_with_none_is_the_caches() {
            let slot = rect(4.0, 32.0, 859.0, 1081.0);
            let mut running = flight();
            running.tiles.push(stacked(wid(1), slot, slot, Some(0), false));
            let fresh = test_picture(ScreenSize::new(859.0, 1000.0));
            let worn = running.wear(vec![
                (wid(1), fresh.clone(), true),
                (wid(5), fresh.clone(), true),
            ]);
            assert_eq!(worn, vec![wid(1)], "a window with no tile is the cache's alone");
            assert_eq!(running.tiles[0].snapshot.coverage, fresh.coverage);
            assert!(running.tiles[0].clipped, "as it was handed");
            assert_eq!(running.tiles.len(), 1);
        }

        #[test]
        fn a_reserved_entrance_joins_with_its_first_picture_once_frontmost_and_focused() {
            let slot = rect(867.0, 32.0, 859.0, 1081.0);
            let neighbour = rect(4.0, 32.0, 859.0, 1081.0);
            let mut running = flight();
            running.tiles.push(stacked(wid(1), neighbour, neighbour, Some(1), false));
            running.entrances.push(PendingEntrance {
                window: wid(2),
                to: slot,
                floating: false,
                worn: Default::default(),
            });
            running.focus = Some(wid(2));
            let tile = running
                .admit(wid(2), &test_picture(slot.size), true, || Some(26.0))
                .expect("admitted");
            assert_eq!(
                (tile.from, tile.to),
                (entrance_from(slot), slot),
                "zero width at its left edge"
            );
            assert!(tile.focused);
            assert!(tile.clipped, "as it was handed");
            assert_eq!(tile.radius, Some(26.0), "as it was handed");
            assert_eq!(tile.depth, 0, "the entrance leads: raised on open");
            assert!(running.entrances.is_empty());
            let other = running.tiles.iter().find(|t| t.window == wid(1)).unwrap();
            assert_eq!(
                other.depth, 2,
                "its neighbour keeps the server's order, within the band"
            );
            let unread = || -> Option<f64> { panic!("a radius read for a window not entering") };
            assert!(
                running.admit(wid(2), &test_picture(slot.size), false, unread).is_none(),
                "taken once"
            );
            assert!(
                running.admit(wid(1), &test_picture(slot.size), false, unread).is_none(),
                "not an entrance"
            );
            assert_eq!(running.tiles.len(), 2, "no second tile");
        }

        /// A stand-in's window is awaited; its real picture, once swapped in, goes onto the same
        /// tile, which moves on as it was.
        #[test]
        fn a_stand_in_takes_its_real_picture_on_the_same_tile() {
            use crate::pictures::platform::picture::{stand_in_image, stand_in_picture};
            let park = rect(1727.0, 32.0, 859.0, 1081.0);
            let slot = rect(4.0, 32.0, 859.0, 1081.0);
            let stand_in = stand_in_picture(slot.size.respace(), stand_in_image().expect("drawn"));
            let request = AnimationRequest {
                window: wid(1),
                server_id: WindowServerId::new(1),
                from: from_overlay_space(park, ScreenRect::zero()),
                to: from_overlay_space(slot, ScreenRect::zero()),
                floating: false,
                worn: Default::default(),
                strip_before: None,
                strip_after: None,
            };
            let flies = FliesOn::Picture {
                source: stand_in.source,
                covered: stand_in.coverage.covered,
            };
            let display = rect(0.0, 0.0, 1728.0, 1117.0);
            assert_eq!(
                EndPicture::of_request(&request, flies, None, request.from, display),
                EndPicture::Awaited
            );
            let mut running = flight();
            let mut tile = stacked(wid(1), park, slot, Some(0), false);
            tile.snapshot = stand_in;
            tile.clipped = true;
            running.tiles.push(tile);
            let real = test_picture(slot.size);
            assert!(matches!(
                running.take_picture(
                    wid(1),
                    &real,
                    false,
                    || panic!("a radius read for a tile"),
                    0.0
                ),
                Took::Tile
            ));
            assert_eq!(running.tiles.len(), 1, "one tile, never a newcomer's entrance");
            let tile = &running.tiles[0];
            assert_eq!((tile.from, tile.to), (park, slot), "travels as it did");
            assert_ne!(tile.snapshot.source, Route::StandIn);
            assert!(!tile.clipped, "drawn whole");
            assert!(running.entrances.is_empty());
        }

        /// A reserved entrance joins with its picture; a window with a tile takes it in place, and
        /// stops flying a focused look; a window nothing draws takes nothing. Only the entrance reads
        /// a radius.
        #[test]
        fn a_swapped_in_picture_admits_an_entrance_or_goes_onto_its_windows_tile() {
            let slot = rect(867.0, 32.0, 859.0, 1081.0);
            let mut running = flight();
            running.tiles.push(stacked(wid(1), slot, slot, Some(0), false));
            running.entrances.push(PendingEntrance {
                window: wid(2),
                to: slot,
                floating: false,
                worn: Default::default(),
            });
            running.focused_look = Some(wid(1));
            let picture = test_picture(slot.size);
            assert!(
                matches!(running.take_picture(wid(2), &picture, false, || None, 0.0), Took::Entrance(tile) if tile.from == entrance_from(slot))
            );
            assert!(matches!(
                running.take_picture(
                    wid(1),
                    &picture,
                    false,
                    || panic!("a radius read for a tile"),
                    0.0
                ),
                Took::Tile
            ));
            assert_eq!(running.focused_look, None);
            assert!(matches!(
                running.take_picture(
                    wid(3),
                    &picture,
                    false,
                    || panic!("a radius read for no tile"),
                    0.0
                ),
                Took::Nothing
            ));
            assert_eq!(running.tiles.len(), 2);
        }

        /// Two columns stepping one column left, frame zero posed.
        fn posed() -> (RunningAnimation, plan::ReflowPlan) {
            let (a, b) = (rect(4.0, 32.0, 859.0, 1081.0), rect(867.0, 32.0, 859.0, 1081.0));
            let shifted = |frame: ScreenRect, dx: f64| {
                rect(
                    frame.origin.x + dx,
                    frame.origin.y,
                    frame.size.width,
                    frame.size.height,
                )
            };
            let requests = [
                (wid(1), a, shifted(a, -863.0), false),
                (wid(2), b, shifted(b, -863.0), false),
            ];
            let mut running = flight();
            running.tiles = requests
                .iter()
                .map(|&(w, from, to, _)| stacked(w, on(from), on(to), Some(0), false))
                .collect();
            running.plan = running.score.plan(plan::reflow_plan(&requests, DISPLAY), &Cue::Plain);
            pose(&mut running);
            let again = requests.map(|(w, _, to, floating)| (w, to, shifted(to, -863.0), floating));
            (running, plan::reflow_plan(&again, DISPLAY))
        }

        fn pose(running: &mut RunningAnimation) {
            let (plan, drawn) = (running.plan.clone(), running.drawn());
            running.score.pose(&stage(&plan, &drawn), plan::Banding::default());
        }

        fn stage<'a>(plan: &'a plan::FlightPlan, drawn: &'a [WindowId]) -> Stage<'a> {
            Stage {
                plan,
                drawn,
                seconds: 0.35,
                cue: Cue::Plain,
                accents: &[],
            }
        }

        fn strip_key(running: &RunningAnimation) -> plan::GroupKey {
            running
                .plan
                .groups
                .iter()
                .find(|g| g.members.iter().any(|m| m.window == wid(1)))
                .map(|g| g.key)
                .expect("rigid")
        }

        /// A push before the flight moves waits for the motion, the latest winning, and plays as the
        /// motion starts; one pushed into a flight that moves plays at once.
        #[test]
        fn a_push_before_the_flight_moves_waits_for_it_and_one_after_plays_at_once() {
            use crate::animation::domain::motion::bounce::Bounce;
            let (mut running, _) = posed();
            let (left, right) = (OverlayVector::new(-72.0, 0.0), OverlayVector::new(72.0, 0.0));
            assert_eq!(running.push(&Cue::Plain, left, 0.35, 10.0), None);
            assert_eq!(running.push(&Cue::Plain, right, 0.35, 10.0), None);
            running.flown = true;
            let (plan, drawn) = (running.plan.clone(), running.drawn());
            let start = running.score.start_moving(&stage(&plan, &drawn), 10.0);
            let key = strip_key(&running);
            let plays = Duration::from_secs_f64(0.35);
            assert!(
                start.contains(&Edit::Add(Bounce::from_rest(right, plays).track(key, 10.0))),
                "the latest push"
            );
            assert!(!start.contains(&Edit::Add(Bounce::from_rest(left, plays).track(key, 10.0))));
            let pushed = running
                .push(&Cue::Plain, OverlayVector::new(0.0, 72.0), 0.35, 10.1)
                .expect("at once");
            assert!(!pushed.is_empty());
            assert!(
                pushed.iter().all(|e| matches!(e, Edit::Add(t) if t.begin() == 10.1)),
                "{pushed:?}"
            );
        }

        /// A pass before the flight moves is folded into frame zero with its step waiting for the
        /// motion, which takes it as it starts; one once the flight moves bends it, the tiles following
        /// the merged plan.
        #[test]
        fn a_pass_before_the_flight_moves_is_folded_with_its_step_waiting_and_one_after_bends_it() {
            use crate::animation::domain::motion::score::Channel;
            let (mut running, again) = posed();
            let step = Accent::Step {
                offset: OverlayVector::new(-576.0, 0.0),
                seconds: 0.35,
            };
            let drawn = running.drawn();
            let pass = Pass {
                plan: &again,
                pan: Some(OverlayVector::new(-863.0, 0.0)),
                focus: None,
                viewport: VIEWPORT,
                drawn: &drawn,
                seconds: 0.35,
                cue: Cue::Plain,
                accent: Some(step),
            };
            assert!(running.take_pass(&pass, 10.0).is_none(), "folded");
            let key = strip_key(&running);
            assert_eq!(
                running.plan.position_of(key),
                OverlayPoint::new(-1726.0, 0.0),
                "both pans from frame zero"
            );
            pose(&mut running);
            running.flown = true;
            let (plan, drawn) = (running.plan.clone(), running.drawn());
            let start = running.score.start_moving(&stage(&plan, &drawn), 10.0);
            assert!(start.iter().any(|e| matches!(e, Edit::Add(t) if t.channel() == Channel::Nudge && t.begin() == 10.0)), "the step waited");

            let further = Pass {
                pan: Some(OverlayVector::new(-863.0, 0.0)),
                accent: None,
                ..pass
            };
            let (delta, edits) = running.take_pass(&further, 10.1).expect("bent");
            assert!(delta.retargeted_groups.iter().any(|(k, _)| *k == key));
            assert!(edits.iter().any(|e| matches!(e, Edit::Add(t) if t.begin() == 10.1)));
            assert_eq!(running.plan.position_of(key), OverlayPoint::new(-2589.0, 0.0));
            let tile = running.tiles.iter().find(|t| t.window == wid(1)).unwrap();
            assert_eq!(
                tile.to.origin.x,
                4.0 - 2589.0,
                "the tile follows the merged plan"
            );
        }

        /// fold1's lab-C folding (`docs/data/2026-10-10-fold`), its repaint held for the corner before
        /// the motion: a frame-zero picture fitting where it starts goes on at once, and the repaint
        /// stays held as the motion starts on a leg in two steps. A pass bending the leg keeps it held
        /// for the new leg's corner only while the old one's is still ahead and the new leg is in two
        /// steps too; one past the old corner becomes the tile's own picture as the new leg is made,
        /// and one before it is let go where the picture from the press covers the new leg.
        #[test]
        fn a_repaint_held_for_the_corner_is_the_tiles_own_once_no_corner_is_ahead() {
            let (column, folded) =
                (rect(866.0, 35.0, 860.0, 1079.0), rect(3.0, 576.0, 1148.0, 538.0));
            let further = rect(3.0, 600.0, 1148.0, 514.0);
            for (at, to, held) in [
                (10.005, further, true),
                (10.1, further, false),
                (10.005, column, false),
            ] {
                let requests = [(wid(1), column, folded, false)];
                let mut running = flight();
                running.tiles = vec![stacked(wid(1), on(column), on(folded), Some(0), false)];
                running.tiles[0].snapshot = test_picture(column.size);
                running.plan =
                    running.score.plan(plan::reflow_plan(&requests, DISPLAY), &Cue::Plain);
                pose(&mut running);
                let at_start = test_picture(column.size);
                assert!(
                    matches!(
                        running.take_picture(wid(1), &at_start, false, || None, 0.0),
                        Took::Tile
                    ),
                    "where it starts"
                );
                let repaint = test_picture(folded.size);
                assert!(matches!(
                    running.take_picture(wid(1), &repaint, false, || None, 0.0),
                    Took::Corner
                ));
                running.flown = true;
                let (plan, drawn) = (running.plan.clone(), running.drawn());
                running.score.start_moving(&stage(&plan, &drawn), 10.0);
                running.turn_corners(10.0);
                let tile = &running.tiles[0];
                assert_eq!(
                    (
                        tile.snapshot.coverage.covered,
                        tile.from_corner.as_ref().map(|p| p.coverage.covered)
                    ),
                    (column.size, Some(folded.size)),
                    "held through the start"
                );

                let again = plan::reflow_plan(&[(wid(1), folded, to, false)], DISPLAY);
                let drawn = running.drawn();
                let pass = Pass {
                    plan: &again,
                    pan: None,
                    focus: None,
                    viewport: VIEWPORT,
                    drawn: &drawn,
                    seconds: 0.35,
                    cue: Cue::Plain,
                    accent: None,
                };
                running.take_pass(&pass, at).expect("bent");
                let tile = &running.tiles[0];
                let carries = (
                    tile.snapshot.coverage.covered,
                    tile.from_corner.as_ref().map(|p| p.coverage.covered),
                );
                let expected = if held {
                    (column.size, Some(folded.size))
                } else {
                    (folded.size, None)
                };
                let expected = if to == column && at < 10.01 {
                    (column.size, None)
                } else {
                    expected
                };
                assert_eq!(carries, expected, "bent at {at} to {to:?}");
            }
        }

        /// lab-C's fold bent back into an unfold on its second step: the new leg runs in two steps
        /// from where the tile is drawn, narrowing first, so the column's repaint landing before that
        /// leg's corner waits for it, though it fits where the flight first started.
        #[test]
        fn a_repaint_landing_after_a_bend_waits_for_the_new_legs_corner() {
            let (column, folded) =
                (rect(866.0, 35.0, 860.0, 1079.0), rect(3.0, 576.0, 1148.0, 538.0));
            let requests = [(wid(1), column, folded, false)];
            let mut running = flight();
            running.tiles = vec![stacked(wid(1), on(column), on(folded), Some(0), false)];
            running.tiles[0].snapshot = test_picture(column.size);
            running.plan = running.score.plan(plan::reflow_plan(&requests, DISPLAY), &Cue::Plain);
            pose(&mut running);
            running.tiles[0].from_corner = Some(test_picture(folded.size));
            running.flown = true;
            let (plan, drawn) = (running.plan.clone(), running.drawn());
            running.score.start_moving(&stage(&plan, &drawn), 10.0);

            let again = plan::reflow_plan(&[(wid(1), folded, column, false)], DISPLAY);
            let drawn = running.drawn();
            let pass = Pass {
                plan: &again,
                pan: None,
                focus: None,
                viewport: VIEWPORT,
                drawn: &drawn,
                seconds: 0.35,
                cue: Cue::Plain,
                accent: None,
            };
            running.take_pass(&pass, 10.1).expect("bent");
            let tile = &running.tiles[0];
            assert_eq!(
                tile.snapshot.coverage.covered, folded.size,
                "past the old corner"
            );
            let corner = running.score.corner_at(wid(1)).expect("the new leg in two steps");
            let repaint = test_picture(column.size);
            assert!(matches!(
                running.take_picture(wid(1), &repaint, false, || None, corner - 0.001),
                Took::Corner
            ));
            let tile = &running.tiles[0];
            assert_eq!(
                (
                    tile.snapshot.coverage.covered,
                    tile.from_corner.as_ref().map(|p| p.coverage.covered)
                ),
                (folded.size, Some(column.size))
            );
            let late = test_picture(column.size);
            assert!(
                matches!(
                    running.take_picture(wid(1), &late, false, || None, corner + 0.001),
                    Took::Tile
                ),
                "past the new corner"
            );
        }

        /// A stand-in flying a fold, stretched with no crop grid to cut, takes its real picture at
        /// once rather than holding it for the corner.
        #[test]
        fn a_stand_in_on_a_fold_takes_its_real_picture_at_once() {
            use crate::pictures::platform::picture::{stand_in_image, stand_in_picture};
            let (column, folded) =
                (rect(866.0, 35.0, 860.0, 1079.0), rect(3.0, 576.0, 1148.0, 538.0));
            assert!(tile::two_steps(column, folded).is_some());
            let mut running = flight();
            let mut tile = stacked(wid(1), on(column), on(folded), Some(0), false);
            tile.snapshot =
                stand_in_picture(folded.size.respace(), stand_in_image().expect("drawn"));
            running.tiles.push(tile);
            let real = test_picture(folded.size);
            assert!(matches!(
                running.take_picture(wid(1), &real, false, || None, 0.0),
                Took::Tile
            ));
            let tile = &running.tiles[0];
            assert!(tile.from_corner.is_none());
            assert_ne!(tile.snapshot.source, Route::StandIn);
        }

        /// Each tile is drawn focused or not on its own. The window left turning unfocused first, focus
        /// still leaves it until the target is drawn focused, which hands focus over.
        #[test]
        fn each_tile_is_drawn_focused_on_its_own_and_focus_hands_over_with_the_target() {
            let slot = rect(4.0, 32.0, 859.0, 1081.0);
            let mut running = flight();
            running.tiles = vec![
                stacked(wid(1), slot, slot, Some(0), false),
                stacked(wid(2), slot, slot, Some(1), false),
                stacked(wid(3), slot, slot, Some(2), false),
            ];
            running.tiles[0].focused = true;
            running.tiles[1].worn = Worn {
                wears_ring: Ring::Active,
                holds_focus: true,
                ..Worn::default()
            };
            running.focus = Some(wid(2));
            running.shown_focus = Some(wid(1));
            let focused = |running: &RunningAnimation| {
                running.tiles.iter().map(|t| t.focused).collect::<Vec<_>>()
            };
            let worn = |running: &RunningAnimation| {
                running.tiles.iter().map(|t| t.worn).collect::<Vec<_>>()
            };
            let ends = worn(&running);
            running.draw_focused(wid(1), false);
            assert_eq!(focused(&running), vec![false, false, false]);
            assert_eq!(
                losing_focus(running.focus, running.shown_focus),
                Some(wid(1)),
                "still leaving it"
            );
            running.draw_focused(wid(2), true);
            assert_eq!(focused(&running), vec![false, true, false]);
            assert_eq!(
                losing_focus(running.focus, running.shown_focus),
                None,
                "a later merge moves none"
            );
            running.draw_focused(wid(4), true);
            assert_eq!(focused(&running), vec![false, true, false], "no tile");
            assert_eq!(worn(&running), ends, "no turn wears anything");
        }

        /// A window wearing its ring where the flight ends is holed over its band as well as over
        /// itself, the band's width in whole device pixels; one wearing none is holed over itself.
        #[test]
        fn a_decorated_window_is_holed_in_its_band_as_well() {
            let slot = rect(4.0, 32.0, 859.0, 1081.0);
            let mut running = flight();
            running.tiles = vec![
                stacked(wid(1), slot, slot, Some(0), false),
                stacked(wid(2), slot, slot, Some(1), false),
            ];
            let band = crate::decorations::domain::style::Band {
                width: 1.3,
                active: crate::decorations::domain::paint::Paint::Solid(
                    crate::decorations::domain::paint::Colour::rgb(0x7fc8ff),
                ),
                inactive: crate::decorations::domain::paint::Paint::Solid(
                    crate::decorations::domain::paint::Colour::rgb(0x505050),
                ),
                urgent: crate::decorations::domain::paint::Paint::Solid(
                    crate::decorations::domain::paint::Colour::rgb(0x9b0000),
                ),
            };
            let decoration = crate::decorations::domain::style::Decoration {
                ring: Some(band),
                ..Default::default()
            };
            running.tiles[0].worn = Worn {
                decoration,
                wears_ring: Ring::Inactive,
                holds_focus: false,
            };
            running.tiles[1].worn = Worn {
                decoration,
                wears_ring: Ring::None,
                holds_focus: false,
            };
            let holes = running.holes(vec![(wid(1), slot), (wid(2), slot), (wid(3), slot)], 2.0);
            assert_eq!(holes, vec![(slot, 1.5), (slot, 0.0), (slot, 0.0)]);
            let bordered = crate::decorations::domain::style::Decoration {
                border: Some(band),
                ..decoration
            };
            running.tiles[0].worn.decoration = bordered;
            running.tiles[1].worn.decoration = bordered;
            let holes = running.holes(vec![(wid(1), slot), (wid(2), slot)], 2.0);
            assert_eq!(
                holes,
                vec![(slot, 3.0), (slot, 1.5)],
                "past the ring outside the border, and past the border"
            );
        }

        #[test]
        fn a_parked_focus_target_flies_its_focused_look_only_where_it_fits() {
            let park = rect(-860.0, 32.0, 859.0, 1081.0);
            let mut running = flight();
            running.tiles.push(stacked(
                wid(2),
                park,
                rect(4.0, 32.0, 859.0, 1081.0),
                Some(0),
                false,
            ));
            assert!(
                !running.fly_focused_look(
                    wid(2),
                    Some(test_picture(ScreenSize::new(600.0, 1081.0))),
                    false
                ),
                "drawn squashed"
            );
            assert!(!running.fly_focused_look(wid(2), None, false));
            assert!(
                !running.fly_focused_look(wid(3), Some(test_picture(park.size)), false),
                "no tile"
            );
            assert_eq!((running.focused_look, running.tiles[0].focused), (None, false));
            let mut look = test_picture(park.size);
            look.coverage.covered.height -= 1.0;
            assert!(running.fly_focused_look(wid(2), Some(look.clone()), true));
            assert_eq!(running.tiles[0].snapshot.coverage, look.coverage);
            assert!(running.tiles[0].clipped, "as it was handed");
            assert!(running.tiles[0].focused);
            assert_eq!(running.focused_look, Some(wid(2)));
        }

        /// A pass is composed from the cache. A tile it retargets keeps the picture the flight gave it
        /// and how focus is drawn on it: its focused look, frame zero's or a swapped-in end picture.
        #[test]
        fn a_retargeted_tile_keeps_its_picture_and_its_focus_while_the_cache_holds_another() {
            let park = rect(-860.0, 32.0, 859.0, 1081.0);
            let slot = rect(4.0, 32.0, 859.0, 1081.0);
            let mut running = flight();
            running.tiles.push(stacked(wid(1), slot, slot, Some(1), false));
            running.tiles.push(stacked(wid(2), park, slot, Some(0), false));
            running.focus = Some(wid(2));
            running.shown_focus = Some(wid(1));
            running.tiles[0].focused = true;
            let mut look = test_picture(park.size);
            look.coverage.covered.height -= 1.0;
            assert!(running.fly_focused_look(wid(2), Some(look.clone()), false));
            let next = rect(867.0, 32.0, 859.0, 1081.0);
            let outcomes =
                running.merge_pass(vec![stacked(wid(2), park, next, Some(0), false)], None);
            assert_eq!(outcomes, vec![(wid(2), Admitted::Retargeted)]);
            let tile = running.tiles.iter().find(|t| t.window == wid(2)).unwrap();
            assert_eq!(tile.to, next);
            assert_eq!(tile.snapshot.coverage, look.coverage, "not the cache's");
            assert!(tile.focused);
            assert_eq!(running.focused_look, Some(wid(2)));
            running.merge_pass(vec![stacked(wid(2), park, slot, Some(0), false)], Some(wid(2)));
            assert_eq!(
                running.focused_look,
                Some(wid(2)),
                "a pass naming the same focus moves none"
            );
        }

        /// A pass moving focus on: the window focused as the flight began is still drawn as its own
        /// turn left it, focused until then, the target before it no longer is, and nothing flies a
        /// focused look until the new target is given its own. Focus moving back to it, or on again,
        /// leaves it as its turn left it.
        #[test]
        fn a_pass_moving_focus_on_redraws_which_tiles_are_drawn_focused() {
            let slot = rect(4.0, 32.0, 859.0, 1081.0);
            let park = rect(-860.0, 32.0, 859.0, 1081.0);
            let mut running = flight();
            running.tiles = vec![
                stacked(wid(1), slot, slot, Some(0), false),
                stacked(wid(2), park, slot, Some(1), false),
            ];
            running.tiles[0].focused = true;
            running.focus = Some(wid(2));
            running.shown_focus = Some(wid(1));
            assert!(running.fly_focused_look(wid(2), Some(test_picture(park.size)), false));
            running.merge_pass(vec![stacked(wid(3), park, slot, Some(2), false)], Some(wid(3)));
            let focused = |running: &RunningAnimation| {
                running
                    .tiles
                    .iter()
                    .map(|t| (t.window, t.focused))
                    .collect::<Vec<(WindowId, bool)>>()
            };
            assert_eq!(focused(&running), vec![
                (wid(1), true),
                (wid(2), false),
                (wid(3), false)
            ]);
            assert_eq!(running.focused_look, None);
            running.draw_focused(wid(1), false);
            running.merge_pass(vec![stacked(wid(4), park, slot, Some(3), false)], Some(wid(4)));
            assert_eq!(
                focused(&running)[0],
                (wid(1), false),
                "turned unfocused with its own picture, it stays so"
            );
            running.merge_pass(vec![stacked(wid(1), slot, slot, Some(0), false)], Some(wid(1)));
            assert_eq!(
                focused(&running)[0],
                (wid(1), false),
                "focus back on it before its focused picture"
            );
            running.merge_pass(vec![stacked(wid(3), park, slot, Some(2), false)], Some(wid(3)));
            assert_eq!(focused(&running)[0], (wid(1), false), "and on again");
        }

        #[test]
        fn a_strip_window_on_this_display_with_no_usable_picture_is_placed_but_not_drawn() {
            use crate::animation::domain::motion::surface::surface_travel;
            use crate::pictures::domain::coverage::needs_capture;
            let mut rng = Gen(95);
            for _ in 0..RUNS {
                let slot: SurfaceRect = rng.on_screen();
                let mut cache: crate::pictures::domain::cache::PictureCache<Picture> =
                    crate::pictures::domain::cache::PictureCache::default();
                cache.insert(wid(2), clipped(slot.size.respace()));
                assert!(cache.usable(wid(1)).is_none(), "seed 95: never captured");
                assert!(cache.usable(wid(2)).is_none(), "seed 95: a sliver");
                assert!(cache.usable(wid(3)).is_none());
                cache.insert(wid(3), test_picture(slot.size));
                assert!(cache.usable(wid(3)).is_some(), "seed 95: the drawn neighbour");

                let (from, to) = surface_travel(
                    slot,
                    SurfacePoint::origin(),
                    SurfacePoint::new(861.0, 0.0),
                    false,
                );
                let mut running = flight();
                running.tiles.push(stacked(wid(3), from, to, Some(0), false));
                let real_to = screen(to);
                running.final_frames =
                    vec![(wid(1), real_to), (wid(2), real_to), (wid(3), real_to)];
                let tiled: Vec<WindowId> = running.tiles.iter().map(|t| t.window).collect();
                let real: HashMap<WindowId, ScreenRect> =
                    running.final_frames.iter().copied().collect();
                let report = handover_report(&running.final_frames, &tiled, &real, DISPLAY);
                // A destination past the edge is a park, which the report excludes (2.3).
                let measured = usize::from(!rini_geometry::is_off_screen(DISPLAY, real_to));
                assert_eq!(
                    report.total, measured,
                    "seed 95: only the drawn window is measured"
                );
                assert_eq!(report.count_over, 0);

                let size: ScreenSize = slot.size.respace();
                assert!(needs_capture(None, size), "seed 95: warmed after the movement");
                assert!(needs_capture(Some(clipped(size).coverage), size));
                assert!(!needs_capture(Some(test_picture(slot.size).coverage), size));
            }
        }
    }

    /// Presses folded into a flight before it moves, each built as the reactor builds it, from the
    /// layout where the press before left it.
    mod folded {
        use super::*;

        const PITCH: f64 = 1117.0;

        fn on_surface(window: WindowId, frame: SurfaceRect, floating: bool) -> SurfaceWindow {
            SurfaceWindow {
                worn: Default::default(),
                window,
                server_id: WindowServerId::new(window.idx.get()),
                frame,
                pinned: false,
                floating,
                from: None,
            }
        }

        /// A strip movement as `start_surface` composes it: every tile travels the viewport's travel,
        /// and the plan is the surface's.
        fn movement(
            windows: &[SurfaceWindow],
            from: SurfacePoint,
            to: SurfacePoint,
        ) -> (Vec<OverlayTile>, plan::ReflowPlan) {
            let tiles = windows
                .iter()
                .map(|w| {
                    let (start, end) = w.travel(from, to);
                    stacked(w.window, start, end, Some(0), w.floating)
                })
                .collect();
            (tiles, plan::surface_plan(windows, from, to))
        }

        fn composed(
            (tiles, pieces): (Vec<OverlayTile>, plan::ReflowPlan),
            focus: WindowId,
        ) -> RunningAnimation {
            let mut running = flight();
            running.tiles = tiles;
            running.plan = running.score.plan(pieces, &Cue::Plain);
            running.focus = Some(focus);
            running
        }

        /// A second press, folded in as `merge_group` folds one into a flight not yet moving.
        fn press(
            running: &mut RunningAnimation,
            windows: &[SurfaceWindow],
            (from, to): (SurfacePoint, SurfacePoint),
            focus: WindowId,
            cue: Cue,
        ) {
            let (tiles, pieces) = movement(windows, from, to);
            running.merge_pass(tiles, Some(focus));
            let drawn = running.drawn();
            let pass = Pass {
                plan: &pieces,
                pan: Some(pan_travel(from, to)),
                focus: running.focus,
                viewport: VIEWPORT,
                drawn: &drawn,
                seconds: 0.35,
                cue,
                accent: None,
            };
            assert!(running.take_pass(&pass, 0.0).is_none(), "folded, not bent");
        }

        fn path(running: &RunningAnimation, window: WindowId) -> (OverlayRect, OverlayRect) {
            let tile = running.tiles.iter().find(|t| t.window == window).expect("drawn");
            (tile.from, tile.to)
        }

        fn piece(
            running: &RunningAnimation,
            window: WindowId,
        ) -> Option<(plan::GroupKey, OverlayVector)> {
            let group = running
                .plan
                .groups
                .iter()
                .find(|g| g.members.iter().any(|m| m.window == window))?;
            Some((group.key, group.travel))
        }

        /// Kai, 2026-10-03: "rapid press navigation messes things up, i periodically see missing
        /// tiles". A switch pressed again before the first moves travels the whole stack as one
        /// surface: the floating window rides its container both rows, and the row the second press
        /// brings starts below the one the first brought, where the stack has it, not on top of it.
        #[test]
        fn a_second_switch_before_the_flight_moves_carries_the_floating_window_with_the_surface() {
            let (a, b, floater, c) = (wid(1), wid(2), wid(3), wid(4));
            let column =
                |row: f64| -> SurfaceRect { rect(4.0, 32.0 + row * PITCH, 1720.0, 1081.0) };
            let floating =
                |row: f64| -> SurfaceRect { rect(224.0, 95.0 + row * PITCH, 600.0, 400.0) };
            let (rest, down) = (SurfacePoint::new(0.0, 0.0), SurfacePoint::new(0.0, PITCH));
            let first = [
                on_surface(a, column(0.0), false),
                on_surface(b, column(1.0), false),
                on_surface(floater, floating(1.0), true),
            ];
            let second = [
                on_surface(b, column(0.0), false),
                on_surface(floater, floating(0.0), true),
                on_surface(c, column(1.0), false),
            ];
            let mut running = composed(movement(&first, rest, down), b);
            press(&mut running, &second, (rest, down), c, Cue::Plain);

            let both = OverlayVector::new(0.0, -2.0 * PITCH);
            assert_eq!(
                running.plan.floating_travel, both,
                "the floating container travels both rows"
            );
            let standing = plan::installed(seen(floating(1.0)));
            assert_eq!(
                running.plan.member(floater),
                Some(plan::Member::Floating { from: standing, to: standing }),
                "riding it, with no motion of its own"
            );
            let strip = piece(&running, a);
            assert_eq!(strip.map(|(_, travel)| travel), Some(both));
            assert_eq!(piece(&running, b), strip);
            assert_eq!(piece(&running, c), strip);
            assert_eq!(
                path(&running, c),
                (seen(column(2.0)), seen(column(0.0))),
                "below B, where the stack has it"
            );
            assert_eq!(path(&running, a).1, seen(column(-2.0)));
            assert_eq!(path(&running, floater).1, seen(floating(-1.0)));
        }

        /// A second step along the strip before the first moves: one piece travels both steps, and
        /// the column the second brings in starts beside the one the first brought.
        #[test]
        fn a_second_pan_before_the_flight_moves_keeps_the_strip_one_piece() {
            let step = 864.0;
            let column = |x: f64| -> SurfaceRect { rect(x, 32.0, 860.0, 1081.0) };
            let columns = [wid(1), wid(2), wid(3), wid(4)];
            let at = |i: usize, scrolled: f64| {
                on_surface(columns[i], column(4.0 + (i as f64 - scrolled) * step), false)
            };
            let first: Vec<SurfaceWindow> = (0..3).map(|i| at(i, 1.0)).collect();
            let second: Vec<SurfaceWindow> = (1..4).map(|i| at(i, 2.0)).collect();
            let (away, rest) = (SurfacePoint::new(-step, 0.0), SurfacePoint::new(0.0, 0.0));
            let mut running = composed(movement(&first, away, rest), columns[2]);
            press(&mut running, &second, (away, rest), columns[3], Cue::Plain);

            let strip = piece(&running, columns[0]);
            assert_eq!(
                strip.map(|(_, travel)| travel),
                Some(OverlayVector::new(-2.0 * step, 0.0))
            );
            for window in &columns[1..] {
                assert_eq!(piece(&running, *window), strip, "{window:?} rides the strip");
            }
            assert_eq!(
                path(&running, columns[3]),
                (seen(column(4.0 + 3.0 * step)), seen(column(4.0 + step))),
                "beside the third"
            );
            assert_eq!(path(&running, columns[0]).1, seen(column(4.0 - 2.0 * step)));
        }

        /// Frame zero shows the latest press folded into it, so the look poses and composes the burst
        /// with that press's cue: a move folded into a plain pass is a move.
        #[test]
        fn a_fold_before_the_flight_moves_shows_the_latest_press() {
            let step = 864.0;
            let column = |x: f64| -> SurfaceRect { rect(x, 32.0, 860.0, 1081.0) };
            let (a, w) = (wid(1), wid(2));
            let first = [
                on_surface(a, column(4.0), false),
                on_surface(w, column(4.0 + step), false),
            ];
            let second = [
                on_surface(w, column(4.0), false),
                on_surface(a, column(4.0 + step), false),
            ];
            let rest = SurfacePoint::new(0.0, 0.0);
            let mut running = composed(movement(&first, rest, rest), a);
            assert_eq!(running.cue, Cue::Plain);
            let moved = Cue::Move {
                window: w,
                direction: rini_ipc::protocol::Direction::Left,
            };
            press(&mut running, &second, (rest, rest), w, moved);
            assert_eq!(running.cue, moved);
            press(&mut running, &second, (rest, rest), w, Cue::Plain);
            assert_eq!(running.cue, Cue::Plain, "the latest, plain or not");
        }
    }

    /// Which tile and which container are drawn in front: `restack` and `band_plan`, over passes.
    mod depth {
        use rini_ipc::protocol::Direction;

        use super::{wid, *};
        use crate::animation::domain::motion::plan::*;
        use crate::animation::domain::motion::surface::SurfaceGeometry;
        use crate::animation::domain::motion::z_group::{GROUP_STRIDE, MAX_TILE_DEPTH};
        use crate::transitions::domain::strip_move::tests::staging::*;

        fn column(i: f64) -> OverlayRect { rect(4.0 + i * 863.0, 32.0, 859.0, 1081.0) }

        /// The plan of `tiles` alone, as a pass plans its requests.
        fn plan_of(tiles: &[OverlayTile]) -> FlightPlan {
            use crate::animation::domain::motion::testing::DISPLAY;
            let requests: Vec<_> = tiles
                .iter()
                .map(|t| {
                    let screen = |r| from_overlay_space(r, DISPLAY);
                    (t.window, screen(t.from), screen(t.to), t.floating)
                })
                .collect();
            FlightPlan::from(reflow_plan(&requests, DISPLAY))
        }

        fn shifted(frame: OverlayRect, dx: f64, dy: f64) -> OverlayRect {
            OverlayRect::new(Point::new(frame.origin.x + dx, frame.origin.y + dy), frame.size)
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
            let mut flight = flight();
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

        /// The 50/50 pair with Settings over them (`z_group::stack`).
        #[test]
        fn band_plan_puts_the_floating_container_behind_the_strip_unless_it_holds_focus() {
            let (left, right) = (rect(4.0, 32.0, 860.0, 1081.0), rect(868.0, 32.0, 856.0, 1081.0));
            let settings = rect(500.0, 300.0, 700.0, 500.0);
            let far = column(2.0);
            let v = OverlayVector::new(-300.0, 0.0);
            // Left half focused: the pair stands, a far column slides, Settings floats.
            let mut tiles = vec![
                stacked(wid(90), left, left, Some(0), false),
                stacked(wid(5830), settings, settings, Some(1), true),
                stacked(wid(89), right, right, Some(2), false),
                stacked(wid(91), far, shifted(far, v.x, v.y), Some(3), false),
            ];
            restack(&mut tiles, Some(wid(90)));
            let plan = plan_of(&tiles);
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
            let plan = plan_of(&tiles);
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
                let plan = plan_of(&tiles);
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
                let (from_offset, to_offset) =
                    (SurfacePoint::new(-step, 0.0), SurfacePoint::new(0.0, 0.0));
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
                VIEWPORT,
            );

            for (window, frame) in second {
                let frame = seen(frame);
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
                .map(|(order, &(window, frame))| {
                    stacked(window, seen(frame), seen(frame), Some(order), false)
                })
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

    /// The engine against the renderer, the reactor, the bar and the pictures service it talks to:
    /// everything it sends them, in the order sent, and the renderer's answers given by the test, on a
    /// clock the test advances. No Core Animation, no window and no live window server.
    mod protocol {
        use std::sync::mpsc::{Receiver as Bursts, TryRecvError};
        use std::sync::{Arc, Mutex};

        use super::*;
        use crate::animation::domain::motion::score::Score;
        use crate::animation::event::Crops;
        use crate::animation::event::Event::{Committed, Unavailable};
        use crate::pictures::platform::service::Command as Asked;
        use crate::transitions::domain::journal::{Burst, Mark, Stamped};
        use crate::windows::domain::info::WindowServerInfo;
        use crate::windows::platform::window_server::FakeWindowServer;

        const CLIENT: Client = Client(1);
        /// The overlay's window, as the renderer reports the window server numbers it.
        const OVERLAY: WindowServerId = WindowServerId(4_242);
        /// The band the bar keeps clear at the display's top.
        const BAND: f64 = 32.0;

        /// Where the pan starts `wid(1)`'s tile, wholly on screen.
        fn start() -> ScreenRect { rect(867.0, 0.0, 859.0, 1081.0) }

        /// Where it ends: at the display's top-left. The window server knows no window a test does not
        /// state, so nothing reads it as anywhere but where its tile ends, and it reads as landed.
        fn end() -> ScreenRect { rect(0.0, 0.0, 859.0, 1081.0) }

        /// One thing the engine sent.
        enum Sent {
            Asked(Asked),
            Drawn(Batch),
            Placed(Vec<(WindowId, ScreenRect)>),
            Flying(bool),
        }

        /// What the engine sent, in order: what it asked of the pictures service is read off before
        /// each other thing it sends, so the two keep the order they were sent in.
        struct Outbox {
            asked: channel::Receiver<Asked>,
            sent: Vec<Sent>,
        }

        impl Outbox {
            fn read_asked(&mut self) {
                while let Ok((_, asked)) = self.asked.try_recv() {
                    self.sent.push(Sent::Asked(asked));
                }
            }

            fn push(&mut self, sent: Sent) {
                self.read_asked();
                self.sent.push(sent);
            }
        }

        /// Puts what it is handed in the outbox, as `sent` makes it.
        fn sink<T: 'static>(
            outbox: &Arc<Mutex<Outbox>>,
            sent: fn(T) -> Sent,
        ) -> impl Fn(T) + Send + 'static {
            let outbox = outbox.clone();
            move |what| outbox.lock().unwrap().push(sent(what))
        }

        struct Harness {
            engine: FlightEngine,
            outbox: Arc<Mutex<Outbox>>,
            now: Arc<Mutex<Instant>>,
            /// What writing the real frames costs on the clock: nothing unless a test says.
            placing: Arc<Mutex<Duration>>,
            /// How far the clock moves each time the engine reads it: not at all unless a test says.
            per_read: Arc<Mutex<Duration>>,
            media: MediaClock,
            /// What `wid(1)` wears through a pan: nothing unless a test says.
            worn: Worn,
            /// The window server the engine reads, which knows no window until a test states it.
            ws: Arc<FakeWindowServer>,
            /// What a framed capture takes of each window, and every window one was taken of.
            captures: Arc<Mutex<Captures>>,
        }

        /// A framed capture of a window answers the picture a test gave for it, else none.
        #[derive(Default)]
        struct Captures {
            of: HashMap<WindowServerId, Picture>,
            taken: Vec<WindowServerId>,
        }

        impl Harness {
            /// An engine on `DISPLAY`, its display drawn, holding a picture of `wid(1)`.
            fn new() -> Harness {
                let (pictures, asked) = Pictures::detached();
                pictures.insert(wid(1), test_picture(end().size));
                let outbox = Arc::new(Mutex::new(Outbox { asked, sent: Vec::new() }));
                let begun = Instant::now();
                let now = Arc::new(Mutex::new(begun));
                let placing = Arc::new(Mutex::new(Duration::ZERO));
                let per_read = Arc::new(Mutex::new(Duration::ZERO));
                let media = MediaClock::anchored(begun, 5_000.0);
                let (tx, rx) = channel::channel();
                let ws = Arc::new(FakeWindowServer::default());
                let captures = Arc::new(Mutex::new(Captures::default()));
                let engine = FlightEngine::new(Parts {
                    rx,
                    tx,
                    pictures,
                    client: CLIENT,
                    draw: Box::new(sink(&outbox, Sent::Drawn)),
                    place_frames: Box::new({
                        let (place, now, placing) =
                            (sink(&outbox, Sent::Placed), now.clone(), placing.clone());
                        move |frames| {
                            *now.lock().unwrap() += *placing.lock().unwrap();
                            place(frames)
                        }
                    }),
                    on_flight: Box::new(sink(&outbox, Sent::Flying)),
                    media,
                    clock: Box::new({
                        let (now, per_read) = (now.clone(), per_read.clone());
                        move || {
                            let mut now = now.lock().unwrap();
                            *now += *per_read.lock().unwrap();
                            *now
                        }
                    }),
                    window_server: ws.clone(),
                    capture: Box::new({
                        let captures = captures.clone();
                        move |_, window, _| {
                            let mut captures = captures.lock().unwrap();
                            captures.taken.push(window);
                            captures.of.get(&window).cloned()
                        }
                    }),
                });
                let mut harness = Harness {
                    engine,
                    outbox,
                    now,
                    placing,
                    per_read,
                    media,
                    worn: Worn::default(),
                    ws,
                    captures,
                };
                let display = harness.step(Event::SetDisplay {
                    id: 1,
                    frame: DISPLAY,
                    scale: 2.0,
                    top_band: BAND,
                    rests: true,
                });
                let committed = harness.committed(only_batch(&display).seq, None);
                harness.step(committed);
                harness
            }

            /// One that measures each lift's handover, as `measure-handover` does, the journal unwritten.
            fn measuring() -> Harness {
                let mut harness = Harness::new();
                harness.step(Event::Journal(JournalSettings {
                    write: false,
                    measure_handover: true,
                }));
                harness
            }

            /// The journal written through a writer whose bursts the test reads, as `Event::Journal`
            /// turning writing on would start the real one. What the ring holds is written at once, the
            /// display's commit, and read off here.
            fn writing(&mut self) -> Bursts<Burst> {
                let (writer, bursts) = Writer::detached();
                if let Some(burst) = self.engine.flights.set_writing(true) {
                    writer.send(burst);
                }
                self.engine.writer = Some(writer);
                self.engine.journal = Some(JournalSettings {
                    write: true,
                    measure_handover: false,
                });
                let ring = bursts.try_recv().expect("the ring, at once");
                assert!(
                    matches!(ring.marks[..], [Stamped { mark: Mark::Drawn { .. }, .. }]),
                    "{ring:?}"
                );
                bursts
            }

            fn at(&self) -> Instant { *self.now.lock().unwrap() }

            /// The window server has `wid(id)` at `frame`.
            fn place(&self, id: u32, frame: ScreenRect) {
                let (pid, id) = (wid(id).pid, WindowServerId::new(id));
                self.ws.set_window(
                    id,
                    Some(WindowServerInfo {
                        id,
                        pid,
                        layer: 0,
                        frame: frame.to_cg(),
                        min_frame: objc2_core_foundation::CGSize::ZERO,
                        max_frame: objc2_core_foundation::CGSize::ZERO,
                    }),
                );
            }

            /// The window server knows no window `wid(id)` any more.
            fn forget(&self, id: u32) { self.ws.set_window(WindowServerId::new(id), None); }

            fn advance(&mut self, by: Duration) { *self.now.lock().unwrap() += by }

            fn generation(&self) -> u64 { self.engine.flights.coordinator().generation().0 }

            /// `event` handled: everything it sent.
            fn step(&mut self, event: Event) -> Vec<Sent> {
                self.engine.handle(event);
                let mut outbox = self.outbox.lock().unwrap();
                outbox.read_asked();
                std::mem::take(&mut outbox.sent)
            }

            /// The renderer's answer that every batch up to `seq` is committed, at once.
            fn committed(&self, seq: u64, overlay: Option<WindowServerId>) -> Event {
                let now = self.at();
                let crops = Crops::default();
                Event::Rendered(Committed {
                    seq,
                    overlay,
                    shown: None,
                    places: false,
                    crops,
                    sent: now,
                    began: now,
                    flushed: now,
                })
            }

            /// The renderer committed every batch up to `seq`.
            fn commit(&mut self, seq: u64) -> Vec<Sent> {
                let committed = self.committed(seq, Some(OVERLAY));
                self.step(committed)
            }

            /// A pan bringing `wid(1)` from `start` to `end`, pressed now.
            fn pan(&self) -> Event {
                self.strip_pan(SurfacePoint::origin(), SurfacePoint::new(867.0, 0.0))
            }

            /// The strip panned on from where `pan` leaves it.
            fn pan_on(&self) -> Event {
                self.strip_pan(SurfacePoint::new(867.0, 0.0), SurfacePoint::new(967.0, 0.0))
            }

            fn strip_pan(&self, from_offset: SurfacePoint, to_offset: SurfacePoint) -> Event {
                let frame = rect(867.0, 0.0, 859.0, 1081.0);
                let window = SurfaceWindow {
                    worn: self.worn,
                    window: wid(1),
                    server_id: WindowServerId::new(1),
                    frame,
                    pinned: false,
                    floating: false,
                    from: None,
                };
                let (_, ends) = window.travel(from_offset, to_offset);
                Event::AnimateSurface {
                    windows: vec![window],
                    from_offset,
                    to_offset,
                    final_frames: vec![(wid(1), screen(ends))],
                    focus: None,
                    focused_before: None,
                    key: None,
                    duration: Duration::from_millis(300),
                    look: Look::default(),
                    cue: Cue::Plain,
                    accent: None,
                    cause: self.cause(),
                }
            }

            /// A push against the strip's end, pressed now.
            fn bounce(&self) -> Event {
                Event::Bounce {
                    windows: Vec::new(),
                    overshoot: OverlayVector::new(-40.0, 0.0),
                    final_frames: Vec::new(),
                    focus: None,
                    duration: Duration::from_millis(200),
                    look: Look::default(),
                    cue: Cue::Plain,
                    cause: self.cause(),
                }
            }

            fn cause(&self) -> Cause {
                Cause {
                    at: self.at(),
                    what: "move_focus",
                }
            }

            /// Frame zero's picture of `wid(1)` where the pan starts it, for the flight running.
            fn frame_zero(&self) -> Event {
                Event::Pictures(crate::pictures::event::Event::Delivered {
                    tag: Tag {
                        client: CLIENT,
                        generation: self.generation(),
                    },
                    why: Why::FrameZero,
                    sent: self.at(),
                    pictures: vec![(wid(1), test_picture(start().size), start())],
                })
            }

            /// A pan pressed and shown 5ms later: the press's batch and the show's, neither committed.
            fn shown(&mut self) -> (u64, u64) {
                let pan = self.pan();
                let begun = only_batch(&self.step(pan)).seq;
                self.advance(Duration::from_millis(5));
                let frame_zero = self.frame_zero();
                (begun, only_batch(&self.step(frame_zero)).seq)
            }

            /// A pan shown, committed, and run to its lift 400ms on: what the tick that lifts sent.
            fn lifted(&mut self) -> Vec<Sent> {
                let (_, shown) = self.shown();
                self.commit(shown);
                self.advance(Duration::from_millis(400));
                let lifted = self.step(Event::Tick);
                assert!(
                    lifted.iter().any(|sent| matches!(sent, Sent::Asked(Asked::Stop(_)))),
                    "the tick lifts: its watches stop"
                );
                lifted
            }
        }

        fn batches(sent: &[Sent]) -> Vec<&Batch> {
            sent.iter()
                .filter_map(|sent| {
                    if let Sent::Drawn(batch) = sent {
                        Some(batch)
                    } else {
                        None
                    }
                })
                .collect()
        }

        fn only_batch(sent: &[Sent]) -> &Batch {
            let batches = batches(sent);
            assert_eq!(batches.len(), 1, "one batch an event");
            batches[0]
        }

        fn placed(sent: &[Sent]) -> Vec<Vec<(WindowId, ScreenRect)>> {
            sent.iter()
                .filter_map(|sent| {
                    if let Sent::Placed(frames) = sent {
                        Some(frames.clone())
                    } else {
                        None
                    }
                })
                .collect()
        }

        fn holds(sent: &[Sent]) -> Vec<Hold> {
            sent.iter()
                .filter_map(|sent| {
                    if let Sent::Asked(Asked::Hold(hold)) = sent {
                        Some(*hold)
                    } else {
                        None
                    }
                })
                .collect()
        }

        fn kind(command: &Render) -> &'static str {
            match command {
                Render::Display { .. } => "display",
                Render::Dress { .. } => "dress",
                Render::Stage { .. } => "stage",
                Render::Play { .. } => "play",
                Render::Swap { .. } => "swap",
                Render::Focus { .. } => "focus",
                Render::Wear { .. } => "wear",
                Render::Holes(_) => "holes",
                Render::Show => "show",
                Render::PlaceRest { .. } => "place_rest",
                Render::Lift { .. } => "lift",
                Render::Trace => "trace",
            }
        }

        fn kinds(batch: &Batch) -> Vec<&'static str> { batch.commands.iter().map(kind).collect() }

        /// The batch holding the lift, among what was sent.
        fn lift_batch(sent: &[Sent]) -> Option<&Batch> {
            batches(sent).into_iter().find(|batch| kinds(batch).contains(&"lift"))
        }

        /// The begin time of every track the batches' plays add.
        fn begins(sent: &[Sent]) -> Vec<f64> {
            let plays = batches(sent).into_iter().flat_map(|batch| &batch.commands);
            let edits = plays.filter_map(|command| {
                if let Render::Play { edits, .. } = command {
                    Some(edits)
                } else {
                    None
                }
            });
            edits
                .flatten()
                .filter_map(|edit| {
                    if let Edit::Add(track) = edit {
                        Some(track.begin())
                    } else {
                        None
                    }
                })
                .collect()
        }

        #[test]
        fn the_frame_zero_ask_is_out_before_the_stage_batch() {
            let mut h = Harness::new();
            let pan = h.pan();
            let sent = h.step(pan);
            let asked = sent.iter().position(|sent| {
                matches!(sent, Sent::Asked(Asked::AskOnScreen(ask)) if ask.moment == Moment::FrameZero)
            });
            let staged = sent.iter().position(
                |sent| matches!(sent, Sent::Drawn(batch) if kinds(batch).contains(&"stage")),
            );
            assert!(asked.expect("frame zero asked") < staged.expect("frame zero posed"));
        }

        #[test]
        fn frame_zero_pictures_and_the_show_are_one_batch() {
            let mut h = Harness::new();
            let pan = h.pan();
            h.step(pan);
            let frame_zero = h.frame_zero();
            let sent = h.step(frame_zero);
            assert_eq!(
                kinds(only_batch(&sent)),
                ["stage", "show", "holes", "play"],
                "posed again with its pictures, holed and moving"
            );
        }

        #[test]
        fn no_frame_is_written_until_the_show_is_committed() {
            let mut h = Harness::new();
            let pan = h.pan();
            let begun = only_batch(&h.step(pan)).seq;
            let frame_zero = h.frame_zero();
            let sent = h.step(frame_zero);
            let shown = only_batch(&sent).seq;
            assert!(placed(&sent).is_empty(), "shown, not drawn");
            assert!(
                placed(&h.commit(begun)).is_empty(),
                "the press's batch shows nothing"
            );
            assert_eq!(placed(&h.commit(shown)), [vec![(wid(1), end())]]);
            assert!(placed(&h.commit(shown)).is_empty(), "written once");
        }

        #[test]
        fn a_merge_after_the_show_is_committed_writes_at_once() {
            let mut h = Harness::new();
            let (_, shown) = h.shown();
            h.commit(shown);
            h.advance(Duration::from_millis(50));
            let pan_on = h.pan_on();
            let sent = h.step(pan_on);
            assert_eq!(
                placed(&sent),
                [vec![(wid(1), rect(-100.0, 0.0, 859.0, 1081.0))]],
                "in the press's own handler"
            );
        }

        #[test]
        fn the_bar_is_not_pictured_until_the_lift_is_drawn() {
            let mut h = Harness::new();
            let sent = h.lifted();
            let lift = lift_batch(&sent).expect("lifted").seq;
            assert!(
                !holds(&sent).contains(&Hold::Settling),
                "the overlay may still be on screen"
            );
            assert_eq!(holds(&h.commit(lift)), [Hold::Settling]);
        }

        /// A background capture of `wid(1)` where the pan ends it, under `generation`.
        fn background(generation: u64, at: Instant) -> Event {
            Event::Pictures(crate::pictures::event::Event::Delivered {
                tag: Tag { client: CLIENT, generation },
                why: Why::Background,
                sent: at,
                pictures: vec![(wid(1), test_picture(end().size), end())],
            })
        }

        #[test]
        fn a_background_picture_delivered_in_flight_is_swapped_onto_its_tile() {
            let mut h = Harness::new();
            let (_, shown) = h.shown();
            h.commit(shown);
            h.advance(Duration::from_millis(50));
            let sent = h.step(background(h.generation(), h.at()));
            assert!(
                batches(&sent).iter().any(|batch| kinds(batch).contains(&"swap")),
                "swapped in"
            );
            assert!(h.engine.delivered.is_empty(), "taken by its tile");
        }

        #[test]
        fn a_background_picture_delivered_after_the_lift_is_not_kept() {
            let mut h = Harness::new();
            h.lifted();
            let sent = h.step(background(h.generation(), h.at()));
            assert!(batches(&sent).is_empty(), "nothing flies to take it");
            assert!(h.engine.delivered.is_empty(), "left to the cache");
        }

        /// A picture taken for where a merge no longer ends its window: no tile takes it, then or later.
        #[test]
        fn a_picture_delivered_for_an_end_a_merge_moved_is_not_kept() {
            let mut h = Harness::new();
            let (_, shown) = h.shown();
            h.commit(shown);
            let before = h.generation();
            let pan_on = h.pan_on();
            h.step(pan_on);
            assert!(h.generation() > before, "the merge bumps the generation");
            let sent = h.step(background(before, h.at()));
            assert!(
                !batches(&sent).iter().any(|batch| kinds(batch).contains(&"swap")),
                "taken for the end before"
            );
            assert!(h.engine.delivered.is_empty(), "left to the cache");
        }

        /// The target's change waiting for the old window to turn is kept until it is swapped in.
        #[test]
        fn the_targets_change_waiting_for_the_old_window_is_kept_until_swapped_in() {
            let mut h = Harness::new();
            let old = rect(40.0, 0.0, 800.0, 1081.0);
            let new = rect(868.0, 0.0, 800.0, 1081.0);
            let window = |id, frame| SurfaceWindow {
                worn: Default::default(),
                window: wid(id),
                server_id: WindowServerId::new(id),
                frame,
                pinned: false,
                floating: false,
                from: None,
            };
            let windows = vec![window(1, old), window(2, new)];
            h.engine.pictures.insert(wid(2), test_picture(new.size));
            let (from_offset, to_offset) = (SurfacePoint::origin(), SurfacePoint::new(20.0, 0.0));
            let final_frames = windows
                .iter()
                .map(|w| (w.window, screen(w.travel(from_offset, to_offset).1)))
                .collect::<Vec<_>>();
            let ends: Vec<ScreenRect> = final_frames.iter().map(|(_, frame)| *frame).collect();
            let press = Event::AnimateSurface {
                windows,
                from_offset,
                to_offset,
                final_frames,
                focus: Some(wid(2)),
                focused_before: Some(wid(1)),
                key: Some(wid(1)),
                duration: Duration::from_millis(300),
                look: Look::default(),
                cue: Cue::Plain,
                accent: None,
                cause: h.cause(),
            };
            h.step(press);
            h.advance(Duration::from_millis(5));
            let frame_zero = h.frame_zero();
            let shown = only_batch(&h.step(frame_zero)).seq;
            h.commit(shown);
            let changed = |h: &Harness, id, frame: ScreenRect| {
                Event::Pictures(crate::pictures::event::Event::Delivered {
                    tag: Tag {
                        client: CLIENT,
                        generation: h.generation(),
                    },
                    why: Why::FocusChanged,
                    sent: h.at(),
                    pictures: vec![(wid(id), test_picture(frame.size), frame)],
                })
            };
            let target = changed(&h, 2, ends[1]);
            h.step(target);
            assert!(
                h.engine.delivered.contains_key(&wid(2)),
                "held for the old window's turn"
            );
            let losing = changed(&h, 1, ends[0]);
            let sent = h.step(losing);
            assert!(h.engine.delivered.is_empty(), "swapped in with it");
            assert!(batches(&sent).iter().any(|batch| batch.commands.iter().any(
                |command| matches!(command, Render::Swap { window, .. } if *window == wid(2))
            )));
        }

        /// A strip pan of `wid(id)` for each of `ids`, side by side from where `pan` starts `wid(1)`.
        fn strip_of(
            h: &Harness,
            ids: &[u32],
            from_offset: SurfacePoint,
            to_offset: SurfacePoint,
        ) -> Event {
            let frames: Vec<(u32, SurfaceRect)> = ids
                .iter()
                .enumerate()
                .map(|(i, &id)| (id, rect(867.0 * (i as f64 + 1.0), 0.0, 859.0, 1081.0)))
                .collect();
            strip_over(h, &frames, from_offset, to_offset)
        }

        /// A strip pan of `wid(id)` at each of `frames` on the surface.
        fn strip_over(
            h: &Harness,
            frames: &[(u32, SurfaceRect)],
            from_offset: SurfacePoint,
            to_offset: SurfacePoint,
        ) -> Event {
            let windows: Vec<SurfaceWindow> = frames
                .iter()
                .map(|&(id, frame)| SurfaceWindow {
                    worn: Default::default(),
                    window: wid(id),
                    server_id: WindowServerId::new(id),
                    frame,
                    pinned: false,
                    floating: false,
                    from: None,
                })
                .collect();
            let final_frames = windows
                .iter()
                .map(|w| (w.window, screen(w.travel(from_offset, to_offset).1)))
                .collect();
            Event::AnimateSurface {
                windows,
                from_offset,
                to_offset,
                final_frames,
                focus: None,
                focused_before: None,
                key: None,
                duration: Duration::from_millis(300),
                look: Look::default(),
                cue: Cue::Plain,
                accent: None,
                cause: h.cause(),
            }
        }

        /// `wid(id)` parked off the display, where a window with no picture flies as the stand-in.
        fn park(h: &Harness, id: u32) { h.place(id, rect(1727.0, 1085.0, 859.0, 1081.0)); }

        /// The tiles every stage and play among `sent` handed the overlay, by window.
        fn handed(sent: &[Sent], window: WindowId) -> Vec<OverlayTile> {
            batches(sent)
                .into_iter()
                .flat_map(|batch| &batch.commands)
                .filter_map(|command| match command {
                    Render::Stage { tiles, .. } | Render::Play { tiles, .. } => Some(tiles),
                    _ => None,
                })
                .flatten()
                .filter(|tile| tile.window == window)
                .cloned()
                .collect()
        }

        /// The `clipped` of every swap among `sent` onto `window`'s tile.
        fn swapped(sent: &[Sent], window: WindowId) -> Vec<bool> {
            batches(sent)
                .into_iter()
                .flat_map(|batch| &batch.commands)
                .filter_map(|command| match command {
                    Render::Swap { window: onto, clipped, .. } if *onto == window => Some(*clipped),
                    _ => None,
                })
                .collect()
        }

        /// The stand-in has no corners of its own, so the overlay clips it to its window's from frame
        /// zero; its window's own picture, captured alone, swaps onto the same tile drawn whole.
        #[test]
        fn a_stand_in_tile_is_clipped_when_staged_and_drawn_whole_once_its_picture_swaps_in() {
            let mut h = Harness::new();
            park(&h, 2);
            let (from_offset, to_offset) = (SurfacePoint::origin(), SurfacePoint::new(867.0, 0.0));
            let press = strip_of(&h, &[1, 2], from_offset, to_offset);
            let staged = handed(&h.step(press), wid(2));
            assert!(!staged.is_empty(), "staged");
            assert!(
                staged.iter().all(|tile| tile.snapshot.source == Route::StandIn && tile.clipped),
                "the stand-in, clipped"
            );
            h.advance(Duration::from_millis(5));
            let frame_zero = h.frame_zero();
            let shown = only_batch(&h.step(frame_zero)).seq;
            h.commit(shown);
            assert!(
                h.engine.running.as_ref().is_some_and(|running| running.flown),
                "moving"
            );
            h.advance(Duration::from_millis(50));
            let lands = rect(867.0, 0.0, 859.0, 1081.0);
            let own = Event::Pictures(crate::pictures::event::Event::Delivered {
                tag: Tag {
                    client: CLIENT,
                    generation: h.generation(),
                },
                why: Why::Background,
                sent: h.at(),
                pictures: vec![(wid(2), test_picture(lands.size), lands)],
            });
            assert_eq!(swapped(&h.step(own), wid(2)), [false], "whole");
        }

        /// Frame zero's pictures are clipped or drawn whole by what each one is, not by what its tile
        /// wore: a picture baked with the desktop behind it is clipped, and one captured alone in
        /// place of a stand-in is drawn whole.
        #[test]
        fn frame_zeros_pictures_are_clipped_by_what_each_one_is() {
            let mut h = Harness::new();
            park(&h, 2);
            let press = strip_of(
                &h,
                &[1, 2],
                SurfacePoint::origin(),
                SurfacePoint::new(867.0, 0.0),
            );
            let pressed = h.step(press);
            assert!(
                handed(&pressed, wid(1)).iter().all(|tile| !tile.clipped),
                "its picture captured alone"
            );
            assert!(
                handed(&pressed, wid(2)).iter().all(|tile| tile.clipped),
                "the stand-in"
            );
            h.advance(Duration::from_millis(5));
            let mut baked = test_picture(start().size);
            baked.source = Route::Baked;
            let frame_zero = Event::Pictures(crate::pictures::event::Event::Delivered {
                tag: Tag {
                    client: CLIENT,
                    generation: h.generation(),
                },
                why: Why::FrameZero,
                sent: h.at(),
                pictures: vec![
                    (wid(1), baked, start()),
                    (wid(2), test_picture(start().size), start()),
                ],
            });
            let sent = h.step(frame_zero);
            let staged = |window| -> Vec<OverlayTile> {
                only_batch(&sent)
                    .commands
                    .iter()
                    .filter_map(|command| {
                        if let Render::Stage { tiles, .. } = command {
                            Some(tiles)
                        } else {
                            None
                        }
                    })
                    .flatten()
                    .filter(|tile| tile.window == window)
                    .cloned()
                    .collect()
            };
            let (one, two) = (staged(wid(1)), staged(wid(2)));
            assert!(!one.is_empty() && !two.is_empty(), "both staged again");
            assert!(
                one.iter().all(|tile| tile.snapshot.source == Route::Baked && tile.clipped),
                "baked"
            );
            assert!(
                two.iter()
                    .all(|tile| tile.snapshot.source == Route::ScreenCaptureKit && !tile.clipped),
                "captured alone"
            );
        }

        /// A tile that joins a moving flight is dressed as it rides in, clipped as any other: a parked
        /// window's stand-in and a picture baked with the desktop behind it; a picture captured alone
        /// is drawn whole.
        #[test]
        fn a_stand_in_joined_by_a_moving_merge_is_clipped() {
            let mut h = Harness::new();
            let (_, shown) = h.shown();
            h.commit(shown);
            h.advance(Duration::from_millis(50));
            park(&h, 2);
            let mut baked = test_picture(end().size);
            baked.source = Route::Baked;
            h.engine.pictures.insert(wid(3), baked);
            let merge = strip_of(
                &h,
                &[1, 2, 3],
                SurfacePoint::new(867.0, 0.0),
                SurfacePoint::new(967.0, 0.0),
            );
            let sent = h.step(merge);
            let joined = |window| {
                let tiles = handed(&sent, window);
                assert!(!tiles.is_empty(), "{window:?} rides in");
                tiles
            };
            assert!(
                joined(wid(2))
                    .iter()
                    .all(|tile| tile.snapshot.source == Route::StandIn && tile.clipped),
                "the stand-in"
            );
            assert!(joined(wid(3)).iter().all(|tile| tile.clipped), "baked");
            assert!(joined(wid(1)).iter().all(|tile| !tile.clipped), "captured alone");
        }

        /// A merge that moves focus onto a parked window flies its focused look, swapped onto its tile
        /// clipped when the look was baked with the desktop behind it and drawn whole when it was not.
        #[test]
        fn a_parked_targets_focused_look_swaps_in_clipped_by_what_it_is() {
            for (route, clipped) in [(Route::Baked, true), (Route::SkyLight, false)] {
                let mut h = Harness::new();
                let (_, shown) = h.shown();
                h.commit(shown);
                h.advance(Duration::from_millis(50));
                park(&h, 2);
                let mut look = test_picture(end().size);
                look.source = route;
                h.engine.pictures.hold_focused(wid(2), look);
                let mut merge = strip_of(
                    &h,
                    &[1, 2],
                    SurfacePoint::origin(),
                    SurfacePoint::new(867.0, 0.0),
                );
                if let Event::AnimateSurface { focus, .. } = &mut merge {
                    *focus = Some(wid(2));
                }
                let sent = h.step(merge);
                assert_eq!(
                    h.engine.running.as_ref().and_then(|running| running.focused_look),
                    Some(wid(2)),
                    "flown"
                );
                assert_eq!(swapped(&sent, wid(2)), [clipped], "{route:?}");
            }
        }

        /// The ring and border of every tile worn again among `sent`, by window.
        fn worn_again(sent: &[Sent]) -> Vec<(WindowId, Worn)> {
            batches(sent)
                .into_iter()
                .flat_map(|batch| &batch.commands)
                .filter_map(|command| {
                    if let Render::Wear { window, worn } = command {
                        Some((*window, *worn))
                    } else {
                        None
                    }
                })
                .collect()
        }

        /// A merge moving focus mid-flight wears the ring and border of each tile already flying as
        /// where the merged flight ends, in the merge's own drain, before any picture turns; one
        /// joining wears them as it is dressed, and a merge that changes no end wears none again.
        #[test]
        fn a_merge_moving_focus_wears_each_flying_tiles_new_end_at_once() {
            let mut h = Harness::new();
            h.worn = Worn {
                wears_ring: Ring::Active,
                holds_focus: true,
                ..Worn::default()
            };
            let (_, shown) = h.shown();
            h.commit(shown);
            h.advance(Duration::from_millis(50));
            let unchanged = h.pan_on();
            assert!(worn_again(&h.step(unchanged)).is_empty(), "the same end");
            park(&h, 2);
            let leaving = Worn::default();
            let gaining = Worn {
                wears_ring: Ring::Active,
                holds_focus: true,
                ..Worn::default()
            };
            let mut merge = strip_of(
                &h,
                &[1, 2],
                SurfacePoint::new(967.0, 0.0),
                SurfacePoint::new(1834.0, 0.0),
            );
            if let Event::AnimateSurface { windows, focus, .. } = &mut merge {
                *focus = Some(wid(2));
                windows[0].worn = leaving;
                windows[1].worn = gaining;
            }
            let sent = h.step(merge);
            assert_eq!(
                worn_again(&sent),
                [(wid(1), leaving)],
                "at once, and only the tile already flying"
            );
            assert!(
                handed(&sent, wid(2)).iter().all(|tile| tile.worn == gaining),
                "dressed with its own"
            );
        }

        /// A layout pass of `wid(1)` from `start` to `end` and of each of `others` from and to its
        /// own frame.
        fn pass_of(h: &Harness, others: &[(u32, ScreenRect, ScreenRect)]) -> Event {
            let mut windows = vec![(1, start(), end())];
            windows.extend_from_slice(others);
            resize_of(h, &windows)
        }

        /// A layout pass moving each of `windows` from its frame to the next, pressed now.
        fn resize_of(h: &Harness, windows: &[(u32, ScreenRect, ScreenRect)]) -> Event {
            let request = |&(id, from, to): &(u32, ScreenRect, ScreenRect)| AnimationRequest {
                worn: Default::default(),
                window: wid(id),
                server_id: WindowServerId::new(id),
                from,
                to,
                floating: false,
                strip_before: None,
                strip_after: None,
            };
            Event::Animate {
                windows: windows.iter().map(request).collect(),
                focus: None,
                focused_before: None,
                key: None,
                duration: Duration::from_millis(300),
                look: Look::default(),
                cause: h.cause(),
            }
        }

        fn report_corners(h: &Harness, id: u32, corners: Option<Vec<f64>>) {
            h.ws.set_corner_radii(WindowServerId::new(id), corners);
        }

        /// Every tile rounds to the radius the window server reports for its window, read as the flight
        /// composes, with nothing learned from a picture: a parked window's stand-in too. A window it
        /// reports none for takes the fallback.
        #[test]
        fn a_layout_pass_and_a_strip_movement_round_every_tile_to_the_radius_the_server_reports() {
            let parked = rect(1727.0, 1085.0, 859.0, 1081.0);
            let slot = rect(867.0, 0.0, 859.0, 1081.0);
            type Compose = fn(&Harness, ScreenRect, ScreenRect) -> Event;
            let composes: [(&str, Compose); 2] = [
                ("a layout pass", |h, parked, slot| {
                    pass_of(h, &[(2, parked, slot)])
                }),
                ("a strip movement", |h, _, _| {
                    strip_of(h, &[1, 2], SurfacePoint::origin(), SurfacePoint::new(867.0, 0.0))
                }),
            ];
            for (what, compose) in composes {
                let mut h = Harness::new();
                report_corners(&h, 1, Some(vec![26.0; 4]));
                report_corners(&h, 2, Some(vec![0.0; 4]));
                park(&h, 2);
                let press = compose(&h, parked, slot);
                let sent = h.step(press);
                let radii = |window| {
                    let tiles = handed(&sent, window);
                    assert!(!tiles.is_empty(), "{what}: {window:?} flies");
                    tiles.iter().map(|tile| tile.radius).collect::<Vec<_>>()
                };
                assert!(
                    radii(wid(1)).iter().all(|radius| *radius == Some(26.0)),
                    "{what}: its own"
                );
                assert!(
                    radii(wid(2)).iter().all(Option::is_none),
                    "{what}: none reported"
                );
                assert!(
                    handed(&sent, wid(2)).iter().all(|tile| tile.snapshot.source == Route::StandIn),
                    "{what}"
                );
            }
        }

        /// Each tile carries what its window wears as its request has it, unread, which its ring and
        /// border show from frame zero, and is drawn focused or not until its turn: the window focus
        /// leaves focused, the target unfocused.
        #[test]
        fn each_tile_is_staged_with_the_decoration_and_ring_its_request_carries() {
            let mut h = Harness::new();
            park(&h, 2);
            let ring = crate::decorations::domain::style::Band {
                width: 4.0,
                active: crate::decorations::domain::paint::Paint::Solid(
                    crate::decorations::domain::paint::Colour::rgb(0x7fc8ff),
                ),
                inactive: crate::decorations::domain::paint::Paint::Solid(
                    crate::decorations::domain::paint::Colour::rgb(0x505050),
                ),
                urgent: crate::decorations::domain::paint::Paint::Solid(
                    crate::decorations::domain::paint::Colour::rgb(0x9b0000),
                ),
            };
            let decoration = crate::decorations::domain::style::Decoration {
                ring: Some(ring),
                ..Default::default()
            };
            let worn = |wears_ring, holds_focus| Worn {
                decoration,
                wears_ring,
                holds_focus,
            };
            let request = |id: u32, from, to, worn| AnimationRequest {
                window: wid(id),
                server_id: WindowServerId::new(id),
                from,
                to,
                floating: false,
                worn,
                strip_before: None,
                strip_after: None,
            };
            let parked = rect(1727.0, 1085.0, 859.0, 1081.0);
            let press = Event::Animate {
                windows: vec![
                    request(1, start(), end(), worn(Ring::None, false)),
                    request(
                        2,
                        parked,
                        rect(867.0, 0.0, 859.0, 1081.0),
                        worn(Ring::Active, true),
                    ),
                ],
                focus: Some(wid(2)),
                focused_before: Some(wid(1)),
                key: Some(wid(1)),
                duration: Duration::from_millis(300),
                look: Look::default(),
                cause: h.cause(),
            };
            let sent = h.step(press);
            let staged = |window| {
                let tiles = handed(&sent, window);
                assert!(!tiles.is_empty(), "{window:?} flies");
                tiles.iter().map(|tile| (tile.worn, tile.focused)).collect::<Vec<_>>()
            };
            let losing = (worn(Ring::None, false), true);
            assert!(
                staged(wid(1)).iter().all(|drawn| *drawn == losing),
                "losing focus"
            );
            let target = (worn(Ring::Active, true), false);
            assert!(staged(wid(2)).iter().all(|drawn| *drawn == target), "the target");
        }

        /// A window that opens has no radius to read until it has drawn: its entrance takes the one the
        /// window server reports as its first picture lands, not the one it reported at the press.
        #[test]
        fn an_entrance_wears_the_radius_read_when_its_picture_lands() {
            let mut h = Harness::new();
            let slot = rect(867.0, 0.0, 859.0, 1081.0);
            let unlaid: ScreenRect = rect(867.0, 0.0, 0.0, 0.0);
            h.place(2, unlaid);
            report_corners(&h, 2, Some(vec![12.0; 4]));
            let pass = pass_of(&h, &[(2, slot, slot)]);
            h.step(pass);
            assert!(
                h.engine
                    .running
                    .as_ref()
                    .is_some_and(|running| running.entrances.iter().any(|e| e.window == wid(2))),
                "reserved"
            );
            h.advance(Duration::from_millis(5));
            let frame_zero = h.frame_zero();
            let shown = only_batch(&h.step(frame_zero)).seq;
            h.commit(shown);
            h.advance(Duration::from_millis(50));
            report_corners(&h, 2, Some(vec![26.0; 4]));
            let landed = Event::Pictures(crate::pictures::event::Event::Delivered {
                tag: Tag {
                    client: CLIENT,
                    generation: h.generation(),
                },
                why: Why::Background,
                sent: h.at(),
                pictures: vec![(wid(2), test_picture(slot.size), slot)],
            });
            let joined = handed(&h.step(landed), wid(2));
            assert!(!joined.is_empty(), "joined");
            assert!(
                joined.iter().all(|tile| tile.radius == Some(26.0)),
                "{:?}",
                joined.iter().map(|t| t.radius).collect::<Vec<_>>()
            );
        }

        /// A flight whose clock has run out lifts only once the window server has each window where
        /// its tile ends.
        #[test]
        fn the_lift_waits_for_the_window_server_to_have_each_window_where_its_tile_ends() {
            let mut h = Harness::new();
            h.place(1, start());
            let (_, shown) = h.shown();
            h.commit(shown);
            h.advance(Duration::from_millis(400));
            assert!(lift_batch(&h.step(Event::Tick)).is_none(), "not where it ends");
            h.place(1, end());
            h.advance(FRAME_INTERVAL);
            assert!(lift_batch(&h.step(Event::Tick)).is_some(), "landed");
        }

        /// A window that opens with no picture flies from the frame macOS shows it at to its slot, on
        /// the one framed capture taken there, frontmost and focused, and the cache keeps that capture.
        #[test]
        fn an_opening_window_travels_from_where_it_opened_on_the_capture_taken_there() {
            let mut h = Harness::new();
            let (spawn, slot) = (rect(300.0, 200.0, 640.0, 480.0), rect(867.0, 0.0, 859.0, 1081.0));
            h.place(2, spawn);
            let taken = WindowServerId::new(2);
            h.captures.lock().unwrap().of.insert(taken, test_picture(spawn.size));
            let press = pass_of(&h, &[(2, slot, slot)]);
            let tiles = handed(&h.step(press), wid(2));
            assert!(!tiles.is_empty(), "it flies");
            assert!(
                tiles.iter().all(|tile| (tile.from, tile.to) == (on(spawn), on(slot))
                    && tile.snapshot.coverage.covered == spawn.size
                    && tile.server_order == Some(0)
                    && tile.focused),
                "{:?}",
                tiles.iter().map(|tile| (tile.from, tile.to)).collect::<Vec<_>>()
            );
            assert_eq!(h.captures.lock().unwrap().taken, [taken], "one capture");
            let running = h.engine.running.as_ref().expect("flying");
            assert!(
                running.entrances.iter().all(|entrance| entrance.window != wid(2)),
                "not reserved"
            );
            assert!(h.engine.pictures.usable(wid(2)).is_some(), "kept");
        }

        /// An opening window whose capture is unusable, or past the pass's budget of captures, is
        /// reserved for its first picture, and the windows past the budget are not captured at all.
        #[test]
        fn an_opening_window_with_no_usable_capture_or_past_the_budget_is_reserved() {
            let mut h = Harness::new();
            let spawn = rect(300.0, 200.0, 640.0, 480.0);
            let opening: Vec<u32> = (2..=7).collect();
            for &id in &opening {
                h.place(id, spawn);
                let capture = if id == 2 {
                    clipped(spawn.size)
                } else {
                    test_picture(spawn.size)
                };
                h.captures.lock().unwrap().of.insert(WindowServerId::new(id), capture);
            }
            let slot = rect(867.0, 0.0, 859.0, 1081.0);
            let others: Vec<_> = opening.iter().map(|&id| (id, slot, slot)).collect();
            let press = pass_of(&h, &others);
            let sent = h.step(press);
            let taken = h.captures.lock().unwrap().taken.clone();
            assert_eq!(
                taken,
                opening[..MAX_SYNC_ENTRANCE_CAPTURES]
                    .iter()
                    .map(|&id| WindowServerId::new(id))
                    .collect::<Vec<_>>(),
                "captured within the budget only"
            );
            let reserved = |id| {
                h.engine
                    .running
                    .as_ref()
                    .is_some_and(|running| running.entrances.iter().any(|e| e.window == wid(id)))
            };
            for id in opening {
                let travels = (3..=5).contains(&id);
                assert_eq!(reserved(id), !travels, "wid({id})");
                assert_eq!(handed(&sent, wid(id)).is_empty(), !travels, "wid({id})");
            }
        }

        /// `windows`' repaints where they end, delivered under the flight's generation now.
        fn repainted(h: &Harness, windows: &[(u32, ScreenRect)]) -> Event {
            Event::Pictures(crate::pictures::event::Event::Delivered {
                tag: Tag {
                    client: CLIENT,
                    generation: h.generation(),
                },
                why: Why::Landed,
                sent: h.at(),
                pictures: windows
                    .iter()
                    .map(|&(id, frame)| (wid(id), test_picture(frame.size), frame))
                    .collect(),
            })
        }

        /// A layout pass moving `wid(1)` as `pass_of` does, and `wid(2)` beside it, shown and moving,
        /// the window server reporting each where the pass starts it.
        fn moving_pair() -> Harness {
            let mut h = Harness::new();
            h.engine.pictures.insert(wid(2), test_picture(end().size));
            let beside = rect(867.0, 0.0, 859.0, 1081.0);
            for (id, frame) in [(1, start()), (2, beside)] {
                h.place(id, frame);
            }
            let press = pass_of(&h, &[(
                2,
                beside,
                beside.translate(ScreenVector::new(-10.0, 0.0)),
            )]);
            h.step(press);
            h.advance(Duration::from_millis(5));
            let frame_zero = h.frame_zero();
            let shown = only_batch(&h.step(frame_zero)).seq;
            h.commit(shown);
            h.advance(COALESCE_WINDOW);
            let ticked = h.step(Event::Tick);
            assert!(kinds(only_batch(&ticked)).contains(&"play"), "moving");
            h.advance(Duration::from_millis(60));
            for id in [1, 2] {
                h.forget(id);
            }
            h
        }

        fn plays(sent: &[Sent]) -> bool {
            batches(sent).iter().any(|batch| kinds(batch).contains(&"play"))
        }

        /// Where the running flight ends `window`'s tile.
        fn tile_to(h: &Harness, window: u32) -> Option<OverlayRect> {
            h.engine.running.as_ref().and_then(|running| {
                running.tiles.iter().find(|tile| tile.window == wid(window)).map(|tile| tile.to)
            })
        }

        /// A pass growing a window while the flight moves writes it at once and draws nothing of it
        /// until its repaint lands: then, in one drain, the tile is bent from where it is drawn at that
        /// moment and the repaint goes onto it.
        #[test]
        fn a_grow_merged_while_the_flight_moves_retargets_once_it_repaints_from_where_its_tile_is_drawn()
         {
            let mut h = moving_pair();
            let grown = rect(0.0, 0.0, 1300.0, 1081.0);
            let press = resize_of(&h, &[(1, end(), grown)]);
            let sent = h.step(press);
            assert!(!plays(&sent), "held");
            assert_eq!(placed(&sent), [vec![(wid(1), grown)]], "written at once");
            h.advance(Duration::from_millis(120));
            let landed = repainted(&h, &[(1, grown)]);
            let sent = h.step(landed);
            let batch = only_batch(&sent);
            let order = kinds(batch);
            let (play, swap) = (
                order.iter().position(|k| *k == "play"),
                order.iter().position(|k| *k == "swap"),
            );
            assert!(
                play.is_some_and(|play| swap.is_some_and(|swap| play < swap)),
                "{order:?}"
            );
            let at = h.media.at(h.at());
            let begins = begins(&sent);
            assert!(
                !begins.is_empty() && begins.iter().all(|begin| (begin - at).abs() < 1e-9),
                "{begins:?} from {at}"
            );
            let to = tile_to(&h, 1);
            assert_eq!(to, Some(on(grown)));
        }

        /// A pass that only shrinks or moves a window while the flight moves bends the flight at once.
        #[test]
        fn a_shrink_merged_while_the_flight_moves_retargets_at_once() {
            let mut h = moving_pair();
            let press = resize_of(&h, &[(1, end(), rect(0.0, 0.0, 600.0, 1081.0))]);
            assert!(plays(&h.step(press)));
        }

        /// The repaint of one of two windows a held pass grows is kept until the other's lands, and
        /// both go onto their tiles as it merges.
        #[test]
        fn a_held_passs_first_repaint_is_kept_until_it_merges() {
            let mut h = moving_pair();
            let (first, second) =
                (rect(0.0, 0.0, 1000.0, 1081.0), rect(700.0, 0.0, 1000.0, 1081.0));
            let press = resize_of(&h, &[
                (1, end(), first),
                (2, rect(857.0, 0.0, 859.0, 1081.0), second),
            ]);
            h.step(press);
            h.advance(Duration::from_millis(80));
            let one = repainted(&h, &[(1, first)]);
            assert!(!plays(&h.step(one)), "still held");
            h.advance(Duration::from_millis(40));
            let other = repainted(&h, &[(2, second)]);
            let sent = h.step(other);
            assert_eq!(swapped(&sent, wid(1)).len(), 1, "kept for the merge");
            assert_eq!(swapped(&sent, wid(2)).len(), 1);
        }

        /// Kai's rapid Alt-R: a press shrinking `wid(1)` on its own sizes, superseding a held grow
        /// whose repaint has not come, ends wider than the picture its tile still carries from the
        /// press: it is held too, and the repaint where it ends goes onto the tile as both merge.
        #[test]
        fn a_shrink_superseding_a_held_grow_ending_wider_than_its_tiles_picture_waits_for_its_repaint()
         {
            let mut h = moving_pair();
            let (grown, narrower) =
                (rect(0.0, 0.0, 1300.0, 1081.0), rect(0.0, 0.0, 1000.0, 1081.0));
            let press = resize_of(&h, &[(1, end(), grown)]);
            h.step(press);
            h.advance(Duration::from_millis(40));
            let press = resize_of(&h, &[(1, grown, narrower)]);
            let sent = h.step(press);
            assert!(!plays(&sent), "held");
            assert_eq!(placed(&sent), [vec![(wid(1), narrower)]], "written at once");
            h.advance(Duration::from_millis(120));
            let landed = repainted(&h, &[(1, narrower)]);
            let sent = h.step(landed);
            let order = kinds(only_batch(&sent));
            let (play, swap) = (
                order.iter().position(|k| *k == "play"),
                order.iter().position(|k| *k == "swap"),
            );
            assert!(
                play.is_some_and(|play| swap.is_some_and(|swap| play < swap)),
                "{order:?}"
            );
            let to = tile_to(&h, 1);
            assert_eq!(to, Some(on(narrower)));
        }

        /// A pass growing nothing that reaches a held one merges with it at once, the held one first:
        /// a window both send somewhere ends where the latest sends it, and stays there as the held
        /// grow's repaint lands.
        #[test]
        fn a_pass_growing_nothing_merges_after_the_one_held_before_it() {
            let mut h = moving_pair();
            let (grown, beside) = (rect(0.0, 0.0, 1300.0, 1081.0), rect(857.0, 0.0, 859.0, 1081.0));
            let (sent, latest) = (rect(869.0, 0.0, 859.0, 1081.0), rect(807.0, 0.0, 859.0, 1081.0));
            let press = resize_of(&h, &[(1, end(), grown), (2, beside, sent)]);
            assert!(!plays(&h.step(press)), "held");
            h.advance(Duration::from_millis(40));
            let press = resize_of(&h, &[(2, sent, latest)]);
            assert!(plays(&h.step(press)), "at once");
            assert!(
                h.engine.running.as_ref().is_some_and(|running| running.held.is_empty()),
                "with the held one"
            );
            assert_eq!(
                (tile_to(&h, 1), tile_to(&h, 2)),
                (Some(on(grown)), Some(on(latest)))
            );
            h.advance(Duration::from_millis(80));
            let landed = repainted(&h, &[(1, grown)]);
            h.step(landed);
            assert_eq!(
                tile_to(&h, 2),
                Some(on(latest)),
                "not sent back where the held one ends it"
            );
        }

        /// A pass only moving a window a held pass grows ends it wider than the picture its tile
        /// carries: held too, and merged once its repaint lands where it moves, onto the moved tile.
        #[test]
        fn a_move_of_a_window_a_held_pass_grows_waits_for_its_repaint() {
            let mut h = moving_pair();
            let (grown, moved) = (rect(0.0, 0.0, 1300.0, 1081.0), rect(100.0, 0.0, 1300.0, 1081.0));
            let press = resize_of(&h, &[(1, end(), grown)]);
            h.step(press);
            h.advance(Duration::from_millis(40));
            let press = resize_of(&h, &[(1, grown, moved)]);
            assert!(!plays(&h.step(press)), "held");
            h.advance(Duration::from_millis(120));
            let landed = repainted(&h, &[(1, moved)]);
            let sent = h.step(landed);
            let order = kinds(only_batch(&sent));
            let (play, swap) = (
                order.iter().position(|k| *k == "play"),
                order.iter().position(|k| *k == "swap"),
            );
            assert!(
                play.is_some_and(|play| swap.is_some_and(|swap| play < swap)),
                "{order:?}"
            );
            assert_eq!(tile_to(&h, 1), Some(on(moved)));
        }

        /// A strip pan carrying a window a held pass grows ends it wider than the picture its tile
        /// carries: held too, not merging every held pass at once, and merged once its repaint lands
        /// where the pan ends it, onto the panned tile.
        #[test]
        fn a_strip_pan_of_a_window_a_held_pass_grows_waits_for_its_repaint() {
            let mut h = moving_pair();
            let grown = rect(0.0, 0.0, 1300.0, 1081.0);
            let press = resize_of(&h, &[(1, end(), grown)]);
            h.step(press);
            h.advance(Duration::from_millis(40));
            h.engine.pictures.insert(wid(1), test_picture(end().size));
            let pan = strip_over(
                &h,
                &[(1, rect(0.0, 0.0, 1300.0, 1081.0))],
                SurfacePoint::origin(),
                SurfacePoint::new(-100.0, 0.0),
            );
            assert!(!plays(&h.step(pan)), "held");
            let panned = rect(100.0, 0.0, 1300.0, 1081.0);
            h.advance(Duration::from_millis(120));
            let landed = repainted(&h, &[(1, panned)]);
            let sent = h.step(landed);
            let order = kinds(only_batch(&sent));
            let (play, swap) = (
                order.iter().position(|k| *k == "play"),
                order.iter().position(|k| *k == "swap"),
            );
            assert!(
                play.is_some_and(|play| swap.is_some_and(|swap| play < swap)),
                "{order:?}"
            );
            assert_eq!(tile_to(&h, 1), Some(on(panned)));
        }

        /// The same shrink once the grow's repaint is on the tile ends inside that picture, and bends
        /// the flight at once.
        #[test]
        fn a_shrink_after_the_grows_repaint_landed_retargets_at_once() {
            let mut h = moving_pair();
            let (grown, narrower) =
                (rect(0.0, 0.0, 1300.0, 1081.0), rect(0.0, 0.0, 1000.0, 1081.0));
            let press = resize_of(&h, &[(1, end(), grown)]);
            h.step(press);
            h.advance(Duration::from_millis(120));
            let landed = repainted(&h, &[(1, grown)]);
            assert_eq!(swapped(&h.step(landed), wid(1)).len(), 1, "on the tile");
            h.advance(Duration::from_millis(40));
            let press = resize_of(&h, &[(1, grown, narrower)]);
            assert!(plays(&h.step(press)));
        }

        /// Before the motion, the same shrink merged into a hold for the grow keeps the overlay holding,
        /// now for the repaint where the shrink ends it, and the motion starts once that lands.
        #[test]
        fn a_shrink_merged_before_the_motion_ending_wider_than_its_tiles_picture_keeps_the_hold() {
            let mut h = Harness::new();
            let (grown, narrower) =
                (rect(0.0, 0.0, 1300.0, 1081.0), rect(0.0, 0.0, 1000.0, 1081.0));
            h.place(1, start());
            let press = resize_of(&h, &[(1, start(), grown)]);
            h.step(press);
            h.advance(Duration::from_millis(5));
            let frame_zero = h.frame_zero();
            let shown = only_batch(&h.step(frame_zero)).seq;
            h.commit(shown);
            h.forget(1);
            h.advance(Duration::from_millis(10));
            let press = resize_of(&h, &[(1, grown, narrower)]);
            assert!(!plays(&h.step(press)));
            h.advance(COALESCE_WINDOW);
            assert!(!plays(&h.step(Event::Tick)), "still holding");
            h.advance(Duration::from_millis(100));
            let landed = repainted(&h, &[(1, narrower)]);
            assert!(plays(&h.step(landed)), "its repaint in, the motion starts");
        }

        /// Where `folding` folds `wid(1)`: wider and shorter, as fold1's lab-C
        /// (`docs/data/2026-10-10-fold`) folded under the window beside it.
        fn folded() -> ScreenRect { rect(0.0, 540.0, 1148.0, 541.0) }

        /// `wid(1)` folding from `start` to `folded`, shown and held for its repaint, the window
        /// server reporting it where the pass starts it until then.
        fn folding() -> Harness {
            let mut h = Harness::new();
            h.place(1, start());
            let press = resize_of(&h, &[(1, start(), folded())]);
            h.step(press);
            h.advance(Duration::from_millis(5));
            let frame_zero = h.frame_zero();
            let shown = only_batch(&h.step(frame_zero)).seq;
            assert!(!plays(&h.commit(shown)), "held for its repaint");
            h.forget(1);
            h
        }

        /// What the plays among `sent` hand the renderer of `window`'s tile: the size of the picture
        /// it flies to the corner, and of the one from it.
        fn carried(sent: &[Sent], window: WindowId) -> Vec<(ScreenSize, Option<ScreenSize>)> {
            batches(sent)
                .into_iter()
                .flat_map(|batch| &batch.commands)
                .filter_map(|command| match command {
                    Render::Play { tiles, .. } => tiles.iter().find(|tile| tile.window == window),
                    _ => None,
                })
                .map(|tile| {
                    (
                        tile.snapshot.coverage.covered,
                        tile.from_corner.as_ref().map(|p| p.coverage.covered),
                    )
                })
                .collect()
        }

        /// The leg every play among `sent` adds `window`'s tile.
        fn legs_of(
            sent: &[Sent],
            window: WindowId,
        ) -> Vec<crate::animation::domain::motion::score::Track> {
            let plays = batches(sent).into_iter().flat_map(|batch| &batch.commands);
            let edits = plays.filter_map(|command| {
                if let Render::Play { edits, .. } = command {
                    Some(edits)
                } else {
                    None
                }
            });
            edits
                .flatten()
                .filter_map(|edit| match edit {
                    Edit::Add(track) if track.node() == Node::Tile(window) => Some(track.clone()),
                    _ => None,
                })
                .collect()
        }

        /// A fold's repaint landing before the motion is not posed at frame zero: the motion starts
        /// in the same drain, its leg in two steps, the tile flying the picture from the press to the
        /// corner and the repaint from it.
        #[test]
        fn a_folds_repaint_before_the_motion_is_flown_from_its_corner_and_never_posed() {
            let mut h = folding();
            h.advance(Duration::from_millis(120));
            let landed = repainted(&h, &[(1, folded())]);
            let sent = h.step(landed);
            let order = kinds(only_batch(&sent));
            assert!(
                !order.contains(&"stage") && !order.contains(&"swap"),
                "{order:?}"
            );
            assert_eq!(carried(&sent, wid(1)), [(start().size, Some(folded().size))]);
            let legs = legs_of(&sent, wid(1));
            assert!(
                legs.len() == 1 && tile::corner_key(&legs[0]).is_some(),
                "{legs:?}"
            );
        }

        /// A fold's repaint landing after its hold ran out, while its tile is still on the first
        /// step, waits for the corner: its leg is installed again as it plays, the tile carrying the
        /// repaint from the corner. One landing past the corner is cut in at once.
        #[test]
        fn a_folds_late_repaint_waits_for_the_corner_until_it_is_past() {
            for (after, waits) in [
                (Duration::from_millis(5), true),
                (Duration::from_millis(100), false),
            ] {
                let mut h = folding();
                h.advance(HOLD_CAP);
                let ticked = h.step(Event::Tick);
                let legs = legs_of(&ticked, wid(1));
                assert!(
                    legs.len() == 1 && tile::corner_key(&legs[0]).is_some(),
                    "moving: {legs:?}"
                );
                let corner =
                    h.engine.running.as_ref().and_then(|running| running.score.corner_at(wid(1)));
                h.advance(after);
                let at = h.media.at(h.at());
                assert_eq!(corner.is_some_and(|corner| at < corner), waits, "{after:?}");
                let landed = repainted(&h, &[(1, folded())]);
                let sent = h.step(landed);
                if waits {
                    assert!(swapped(&sent, wid(1)).is_empty(), "{after:?}: not cut in");
                    assert_eq!(legs_of(&sent, wid(1)), legs, "{after:?}: its leg again");
                    assert_eq!(carried(&sent, wid(1)), [(start().size, Some(folded().size))]);
                } else {
                    assert_eq!(swapped(&sent, wid(1)).len(), 1, "{after:?}: cut in");
                    assert!(legs_of(&sent, wid(1)).is_empty());
                }
            }
        }

        /// A fold whose repaint is held for the corner, shrunk further by a press before the corner:
        /// the merged pass is judged against that repaint, which covers the new end, so the window is
        /// kept and the pass merges at once, not held as a grow past the picture from the press.
        #[test]
        fn a_fold_shrunk_further_before_its_corner_is_judged_against_its_held_repaint() {
            let mut h = folding();
            h.advance(HOLD_CAP);
            assert!(plays(&h.step(Event::Tick)), "moving");
            h.advance(Duration::from_millis(5));
            let landed = repainted(&h, &[(1, folded())]);
            assert!(
                swapped(&h.step(landed), wid(1)).is_empty(),
                "held for the corner"
            );
            let press = resize_of(&h, &[(1, folded(), rect(0.0, 564.0, 1148.0, 517.0))]);
            assert!(plays(&h.step(press)), "merged at once");
            let running = h.engine.running.as_ref().expect("running");
            assert!(running.held.is_empty(), "nothing held");
        }

        #[test]
        fn the_quiet_counts_from_the_lift_being_drawn() {
            let mut h = Harness::new();
            let lift = lift_batch(&h.lifted()).expect("lifted").seq;
            assert!(h.engine.quiet.is_none(), "not armed at the lift's call");
            h.commit(lift);
            assert_eq!(
                h.engine.quiet.as_ref().map(|(generation, _)| *generation),
                Some(Generation(1))
            );
        }

        /// `wid(1)` wearing a 1.3pt ring where the flight ends, 1.5pt at 2x.
        fn ringed() -> Worn {
            use crate::decorations::domain::paint::{Colour, Paint};
            let solid = Paint::Solid(Colour::rgb(0x7fc8ff));
            let ring = crate::decorations::domain::style::Band {
                width: 1.3,
                active: solid,
                inactive: solid,
                urgent: solid,
            };
            let decoration = crate::decorations::domain::style::Decoration {
                ring: Some(ring),
                ..Default::default()
            };
            Worn {
                decoration,
                wears_ring: Ring::Active,
                holds_focus: true,
            }
        }

        fn holes(sent: &[Sent]) -> Vec<f64> {
            let commands = batches(sent).into_iter().flat_map(|batch| &batch.commands);
            commands
                .filter_map(|command| {
                    if let Render::Holes(holes) = command {
                        Some(holes)
                    } else {
                        None
                    }
                })
                .flat_map(|holes| holes.iter().map(|(_, outset)| *outset))
                .collect()
        }

        /// The overlay is holed over a ringed window's band too, as wide as the band is drawn on the
        /// display; with no display laid out it knows no scale, and holes the window alone.
        #[test]
        fn the_overlay_is_holed_over_a_band_at_its_displays_scale_and_over_the_window_alone_without_one()
         {
            for (displayed, outset) in [(true, 1.5), (false, 0.0)] {
                let mut h = Harness::new();
                h.worn = ringed();
                let pan = h.pan();
                h.step(pan);
                if !displayed {
                    h.engine.display = None;
                }
                h.advance(Duration::from_millis(5));
                let frame_zero = h.frame_zero();
                assert_eq!(holes(&h.step(frame_zero)), [outset], "displayed {displayed}");
            }
        }

        /// measure-handover measures past a window's edge as far as the band it wears where the flight
        /// ends reaches, in whole device pixels.
        #[test]
        fn a_measured_lift_hands_each_window_the_band_it_wears() {
            let mut h = Harness::measuring();
            h.worn = ringed();
            let check = h.lifted().into_iter().find_map(|sent| match sent {
                Sent::Asked(Asked::CheckHandover(check)) => Some(check),
                _ => None,
            });
            assert_eq!(check.expect("measured").windows, [(wid(1), end(), 1.5)]);
        }

        #[test]
        fn a_measured_lift_holds_its_lift_and_the_bar_until_the_journal_hides_it() {
            let taken = |h: &mut Harness| {
                let tag = Tag {
                    client: CLIENT,
                    generation: h.generation(),
                };
                h.step(Event::Pictures(crate::pictures::event::Event::OverlayTaken {
                    tag,
                    taken: true,
                }))
            };
            let patience = |h: &mut Harness| {
                h.advance(OVERLAY_TAKE_PATIENCE);
                h.step(Event::LiftPatience)
            };
            let moved = |h: &mut Harness| {
                h.step(Event::SetDisplay {
                    id: 1,
                    frame: DISPLAY,
                    scale: 2.0,
                    top_band: 0.0,
                    rests: true,
                })
            };
            type Hearing<'a> = (&'a str, &'a dyn Fn(&mut Harness) -> Vec<Sent>);
            let hearings: [Hearing<'_>; 3] = [
                ("its picture", &taken),
                ("the patience", &patience),
                ("the overlay moving", &moved),
            ];
            for (what, hears) in hearings {
                let mut h = Harness::measuring();
                let sent = h.lifted();
                let check = sent.iter().find_map(|sent| match sent {
                    Sent::Asked(Asked::CheckHandover(check)) => Some(check.clone()),
                    _ => None,
                });
                let check = check.expect("measured");
                assert_eq!(check.overlay, OVERLAY, "the window the renderer reported");
                assert_eq!(
                    check.overlay_origin,
                    window_origin(DISPLAY, BAND),
                    "the display last sent"
                );
                assert_eq!(check.windows, [(wid(1), end(), 0.0)], "wearing no ring, no band");
                assert!(lift_batch(&sent).is_none(), "held");
                assert!(!holds(&sent).contains(&Hold::Settling), "held with the lift");

                let sent = hears(&mut h);
                let batch = only_batch(&sent);
                let lift = kinds(batch)
                    .iter()
                    .position(|kind| *kind == "lift")
                    .unwrap_or_else(|| panic!("{what}: hidden"));
                if let Some(display) = kinds(batch).iter().position(|kind| *kind == "display") {
                    assert!(lift < display, "{what}: hidden before the overlay moves");
                }
                assert!(
                    !holds(&sent).contains(&Hold::Settling),
                    "{what}: owed to the hide"
                );
                assert!(h.engine.quiet.is_none(), "{what}: owed to the hide");
                let seq = batch.seq;
                assert_eq!(holds(&h.commit(seq)), [Hold::Settling], "{what}");
                assert!(h.engine.quiet.is_some(), "{what}");
            }

            let mut h = Harness::measuring();
            h.lifted();
            h.advance(Duration::from_millis(10));
            let pan = h.pan();
            let sent = h.step(pan);
            assert_eq!(
                kinds(only_batch(&sent))[0],
                "lift",
                "a press never waits for the overlay's picture"
            );
            assert_eq!(holds(&sent), [Hold::Transition]);
        }

        #[test]
        fn a_press_before_the_lift_is_drawn_keeps_its_hold() {
            let mut h = Harness::new();
            let lift = lift_batch(&h.lifted()).expect("lifted").seq;
            h.advance(Duration::from_millis(10));
            let pan = h.pan();
            let pressed = h.step(pan);
            assert_eq!(holds(&pressed), [Hold::Transition]);
            let drawn = h.commit(lift);
            assert!(
                holds(&drawn).is_empty(),
                "the last lift's bar is not pictured under the new flight"
            );
            assert!(h.engine.quiet.is_none(), "nor its quiet armed");
        }

        #[test]
        fn a_flight_is_not_settled_while_a_batch_is_unacknowledged() {
            let mut h = Harness::new();
            let (begun, shown) = h.shown();
            h.commit(begun);
            h.advance(Duration::from_millis(400));
            let sent = h.step(Event::Tick);
            assert!(lift_batch(&sent).is_none(), "the show's batch is not drawn");
            let placed = only_batch(&sent).seq;
            h.commit(shown);
            h.advance(FRAME_INTERVAL);
            assert!(
                lift_batch(&h.step(Event::Tick)).is_none(),
                "nor the rest windows' place"
            );
            h.commit(placed);
            h.advance(FRAME_INTERVAL);
            assert!(lift_batch(&h.step(Event::Tick)).is_some());
        }

        #[test]
        fn an_event_that_draws_nothing_sends_no_batch() {
            let mut h = Harness::new();
            let (begun, shown) = h.shown();
            assert!(batches(&h.commit(begun)).is_empty(), "a commit");
            assert!(
                batches(&h.commit(shown)).is_empty(),
                "a commit releasing the writes"
            );
            h.advance(FRAME_INTERVAL);
            assert!(batches(&h.step(Event::Tick)).is_empty(), "a tick");
            assert!(
                batches(&h.step(Event::LooksTurned(vec![(wid(1), end())]))).is_empty(),
                "a turned look"
            );
        }

        #[test]
        fn every_track_begins_on_the_lifecycle_clock() {
            let mut h = Harness::new();
            let pan = h.pan();
            h.step(pan);
            h.advance(Duration::from_millis(7));
            let delivered = h.at();
            let frame_zero = h.frame_zero();
            let started = begins(&h.step(frame_zero));
            assert!(!started.is_empty());
            assert!(
                started.iter().all(|begin| *begin == h.media.at(delivered)),
                "{started:?}: the motion at the drive's instant"
            );
            h.advance(Duration::from_millis(60));
            let pushed = h.at();
            let bounce = h.bounce();
            let bounced = begins(&h.step(bounce));
            assert!(!bounced.is_empty());
            assert!(
                bounced.iter().all(|begin| *begin == h.media.at(pushed)),
                "{bounced:?}: a push at its press's"
            );
        }

        /// One reading an event would mark the compose and frame zero's wait for its show as taking
        /// no time: each is two drives in one handler.
        #[test]
        fn each_drive_reads_the_clock_as_it_is_driven() {
            let mut h = Harness::new();
            let bursts = h.writing();
            *h.per_read.lock().unwrap() = Duration::from_micros(10);
            let (_, shown) = h.shown();
            h.commit(shown);
            h.advance(Duration::from_millis(400));
            let lift = lift_batch(&h.step(Event::Tick)).expect("lifted").seq;
            h.commit(lift);
            h.step(Event::Quiet(Generation(h.generation())));
            let burst = bursts.try_recv().expect("the settled burst");
            let at = |wanted: fn(&Mark) -> bool| {
                burst
                    .marks
                    .iter()
                    .find(|stamped| wanted(&stamped.mark))
                    .map(|stamped| stamped.at)
                    .expect("marked")
            };
            let begin = at(|mark| matches!(mark, Mark::Begin { .. }));
            let composed = at(|mark| matches!(mark, Mark::Merge { moved: false, .. }));
            assert!(begin < composed, "the compose takes its time");
            let delivered = at(|mark| matches!(mark, Mark::Delivery { why: Why::FrameZero, .. }));
            let show = at(|mark| matches!(mark, Mark::Show));
            assert!(delivered < show, "frame zero's wait for its show takes its time");
        }

        /// The lift before a flight, a lift held for the overlay's picture too, let go of the desktop
        /// behind the tiles: the flight is dressed after it, with the desktop the service holds.
        #[test]
        fn a_flight_after_a_lift_is_dressed_again() {
            for mut h in [Harness::new(), Harness::measuring()] {
                let display = Display {
                    id: 1,
                    frame: DISPLAY,
                    scale: 2.0,
                };
                h.engine.pictures.hold_desktop(display, test_picture(DISPLAY.size));
                let mut sent = h.lifted();
                h.advance(Duration::from_millis(10));
                let pan = h.pan();
                sent.extend(h.step(pan));
                let commands: Vec<&Render> =
                    batches(&sent).into_iter().flat_map(|batch| &batch.commands).collect();
                let lift = commands
                    .iter()
                    .rposition(|command| matches!(command, Render::Lift { .. }))
                    .expect("lifted");
                let dress = commands
                    .iter()
                    .rposition(|command| matches!(command, Render::Dress { .. }))
                    .expect("dressed");
                assert!(lift < dress, "dressed after the lift");
                assert!(
                    matches!(commands[dress], Render::Dress { desktop: Some(_), .. }),
                    "with the desktop held"
                );
            }
        }

        /// The tick that sees the windows landed a frame before the clock runs out sends the rest
        /// windows' place in a batch of its own, so the lift's batch only hides.
        #[test]
        fn a_landed_flight_places_the_rest_a_tick_before_it_lifts() {
            let mut h = Harness::new();
            let (_, shown) = h.shown();
            h.commit(shown);
            let left = h.engine.flights.coordinator().remaining(h.at()).expect("moving");
            let tick = |h: &mut Harness, by: Duration| {
                h.advance(by);
                let sent = h.step(Event::Tick);
                if let Some(seq) = batches(&sent).iter().map(|batch| batch.seq).max() {
                    h.commit(seq);
                }
                sent
            };
            assert!(
                !batches(&tick(&mut h, left - Duration::from_millis(120)))
                    .iter()
                    .any(|batch| kinds(batch).contains(&"place_rest"))
            );
            let sent = tick(&mut h, Duration::from_millis(115));
            assert_eq!(kinds(only_batch(&sent)), ["place_rest"], "a tick ahead");
            let sent = tick(&mut h, FRAME_INTERVAL);
            let lift = lift_batch(&sent).expect("lifted");
            assert!(!kinds(lift).contains(&"place_rest"), "{:?}", kinds(lift));
        }

        /// A place and a lift carry the latest press the engine had taken as it gave them, so what is
        /// drawn at rest tells a press after them, a flight of its own, from one merged into theirs;
        /// a lift held for the overlay's picture and hidden by the next press carries the press before.
        #[test]
        fn a_place_and_a_lift_carry_the_latest_press_taken_before_them() {
            let through = |sent: &[Sent]| -> Vec<(&'static str, Option<Instant>)> {
                batches(sent)
                    .into_iter()
                    .flat_map(|batch| &batch.commands)
                    .filter_map(|command| match command {
                        Render::PlaceRest { through } => Some(("place_rest", *through)),
                        Render::Lift { through } => Some(("lift", *through)),
                        _ => None,
                    })
                    .collect()
            };
            let mut h = Harness::new();
            let pressed = Some(h.at());
            let (_, shown) = h.shown();
            h.commit(shown);
            let left = h.engine.flights.coordinator().remaining(h.at()).expect("moving");
            h.advance(left - Duration::from_millis(5));
            let placed = h.step(Event::Tick);
            h.commit(only_batch(&placed).seq);
            h.advance(FRAME_INTERVAL);
            let lifted = h.step(Event::Tick);
            assert_eq!([through(&placed), through(&lifted)].concat(), [
                ("place_rest", pressed),
                ("lift", pressed)
            ]);

            let mut h = Harness::measuring();
            let pressed = Some(h.at());
            h.lifted();
            h.advance(Duration::from_millis(10));
            let pan = h.pan();
            let hidden = h.step(pan);
            assert_eq!(
                through(&hidden),
                [("lift", pressed)],
                "the held lift's press, not the one hiding it"
            );
        }

        /// With nothing drawn at rest no place is sent, so the lift waits behind no batch of its own,
        /// and no tick reads the windows' landing a frame before the clock runs out.
        #[test]
        fn with_nothing_drawn_at_rest_no_place_is_sent_and_the_lift_waits_for_none() {
            let mut h = Harness::new();
            let display = h.step(Event::SetDisplay {
                id: 1,
                frame: DISPLAY,
                scale: 2.0,
                top_band: BAND,
                rests: false,
            });
            h.commit(only_batch(&display).seq);
            let (_, shown) = h.shown();
            h.commit(shown);
            let left = h.engine.flights.coordinator().remaining(h.at()).expect("moving");
            h.advance(left - Duration::from_millis(5));
            assert!(
                !h.engine.flights.coordinator().wants_landing(h.at()),
                "the landing read only at the end"
            );
            assert!(batches(&h.step(Event::Tick)).is_empty(), "no place a tick ahead");
            h.advance(FRAME_INTERVAL);
            let sent = h.step(Event::Tick);
            let lift = lift_batch(&sent).expect("lifted, with no place to wait for");
            assert!(
                batches(&sent).iter().all(|batch| !kinds(batch).contains(&"place_rest")),
                "{:?}",
                kinds(lift)
            );
        }

        #[test]
        fn a_stage_the_renderer_cannot_draw_is_abandoned_and_written() {
            let mut h = Harness::new();
            let pan = h.pan();
            let begun = only_batch(&h.step(pan)).seq;
            let frame_zero = h.frame_zero();
            let shown = only_batch(&h.step(frame_zero)).seq;
            let sent = h.step(Event::Rendered(Unavailable { seq: begun }));
            assert_eq!(
                placed(&sent),
                [vec![(wid(1), end())]],
                "at once: the show sent since draws nothing"
            );
            assert_eq!(kinds(only_batch(&sent)), ["lift"]);
            assert_eq!(holds(&sent), [Hold::Released]);
            assert!(sent.iter().any(|sent| matches!(sent, Sent::Flying(false))));
            assert!(!h.engine.flights.coordinator().is_flying());
            let sent = h.step(Event::Rendered(Unavailable { seq: shown }));
            assert!(
                placed(&sent).is_empty() && batches(&sent).is_empty(),
                "abandoned once"
            );
        }

        #[test]
        fn a_lift_the_renderer_cannot_draw_still_writes_and_settles() {
            let mut h = Harness::new();
            h.shown();
            h.advance(Duration::from_millis(300) + LIFT_GRACE + FRAME_INTERVAL);
            let sent = h.step(Event::Tick);
            let lift = lift_batch(&sent).expect("overdue, lifted with nothing drawn").seq;
            assert!(placed(&sent).is_empty(), "the show is not drawn");
            let sent = h.step(Event::Rendered(Unavailable { seq: lift }));
            assert_eq!(
                placed(&sent),
                [vec![(wid(1), end())]],
                "nothing will be drawn: written at once"
            );
            assert_eq!(holds(&sent), [Hold::Settling]);
            let quiet = h.engine.quiet.as_ref().map(|(generation, _)| *generation);
            assert_eq!(quiet, Some(Generation(1)), "the quiet armed");
            let sent = h.step(Event::Quiet(Generation(1)));
            assert!(
                sent.iter().any(|sent| matches!(sent, Sent::Flying(false))),
                "settled"
            );
        }

        #[test]
        fn the_renderer_replica_and_the_score_agree() {
            let mut h = Harness::new();
            let pan = h.pan();
            let mut sent = h.step(pan);
            h.advance(Duration::from_millis(5));
            let frame_zero = h.frame_zero();
            sent.extend(h.step(frame_zero));
            h.advance(Duration::from_millis(50));
            let pan_on = h.pan_on();
            sent.extend(h.step(pan_on));
            h.advance(Duration::from_millis(30));
            let bounce = h.bounce();
            sent.extend(h.step(bounce));
            let mut replica = Score::default();
            for command in batches(&sent).into_iter().flat_map(|batch| &batch.commands) {
                match command {
                    Render::Stage { score, .. } => replica = score.clone(),
                    Render::Play { edits, .. } => edits.iter().for_each(|edit| replica.apply(edit)),
                    _ => {}
                }
            }
            let running = h.engine.running.as_ref().expect("flying");
            assert_eq!(&replica, running.score.score());
        }

        /// The journal says when a commit released what waited for it: the commit is marked as the
        /// engine hears it, before the writes it releases take their time.
        #[test]
        fn a_committed_batch_is_journalled_before_what_it_releases() {
            let mut h = Harness::new();
            let bursts = h.writing();
            *h.placing.lock().unwrap() = Duration::from_millis(3);
            let pan = h.pan();
            h.step(pan);
            h.advance(Duration::from_millis(5));
            let frame_zero = h.frame_zero();
            let sent = h.step(frame_zero);
            let (shown, handed) = (only_batch(&sent).seq, only_batch(&sent).sent);
            assert_eq!(handed, h.at(), "stamped as it is sent");
            h.advance(Duration::from_micros(1_500));
            let heard = h.at();
            let at = |us| handed + Duration::from_micros(us);
            let crops = Crops {
                fills: 1,
                ground: Duration::from_micros(7_910),
            };
            let answer = Committed {
                seq: shown,
                overlay: Some(OVERLAY),
                shown: Some(true),
                places: false,
                crops,
                sent: handed,
                began: at(400),
                flushed: at(700),
            };
            assert_eq!(placed(&h.step(Event::Rendered(answer))), [vec![(wid(1), end())]]);
            assert_eq!(
                h.at(),
                heard + Duration::from_millis(3),
                "the writes took their time"
            );

            h.advance(Duration::from_millis(400));
            let lift = lift_batch(&h.step(Event::Tick)).expect("lifted").seq;
            h.commit(lift);
            h.step(Event::Quiet(Generation(h.generation())));
            let burst = bursts.try_recv().expect("the settled burst");
            let drawn: Vec<_> = burst
                .marks
                .iter()
                .filter(|stamped| matches!(stamped.mark, Mark::Drawn { seq, .. } if seq == shown))
                .collect();
            assert_eq!(drawn.len(), 1, "{:?}", burst.marks);
            assert_eq!(drawn[0].at, heard, "before the writes it released");
            let mark = Mark::Drawn {
                seq: shown,
                shown: Some(true),
                places: false,
                crops: 1,
                ground_us: 7_910,
                queued_us: 400,
                applied_us: 300,
                flushed_before_us: 800,
            };
            assert_eq!(drawn[0].mark, mark);
        }

        /// A slow main-thread turn the observer sends is journalled with the flight it falls in, and draws
        /// nothing.
        #[test]
        fn a_main_turn_is_journalled_and_draws_nothing() {
            let mut h = Harness::new();
            let bursts = h.writing();
            let pan = h.pan();
            h.step(pan);
            let began = h.at();
            let ms = Duration::from_millis;
            let turn = rini_runloop::turns::MainTurn {
                began,
                busy: ms(260),
                actors: vec![("renderer", ms(2))],
                outside: ms(258),
                changes: vec![("overlay.show", began - ms(4))],
            };
            assert!(batches(&h.step(Event::MainTurn(turn))).is_empty());
            let frame_zero = h.frame_zero();
            let shown = only_batch(&h.step(frame_zero)).seq;
            h.commit(shown);
            h.advance(Duration::from_millis(400));
            let lift = lift_batch(&h.step(Event::Tick)).expect("lifted").seq;
            h.commit(lift);
            h.step(Event::Quiet(Generation(h.generation())));
            let burst = bursts.try_recv().expect("the settled burst");
            let at = burst
                .marks
                .iter()
                .position(|stamped| matches!(stamped.mark, Mark::MainTurn { .. }))
                .expect("marked");
            let marks: Vec<_> = burst.marks[at..at + 3]
                .iter()
                .map(|stamped| (stamped.at, stamped.mark))
                .collect();
            assert_eq!(
                marks,
                [
                    Mark::MainTurn {
                        busy_us: 260_000,
                        outside_us: 258_000,
                        actors: 1,
                        changes: 1
                    },
                    Mark::TurnActor { actor: "renderer", us: 2_000 },
                    Mark::TurnChange {
                        what: "overlay.show",
                        before_us: 4_000
                    },
                ]
                .map(|mark| (began, mark))
            );
        }

        /// Every picture a tile takes is journalled with its route, its blur and its age: the cache's as
        /// the flight composes, frame zero's before the show, and one swapped in mid-flight.
        #[test]
        fn every_picture_a_tile_takes_is_journalled_with_its_route_blur_and_age() {
            let mut h = Harness::new();
            let bursts = h.writing();
            let pan = h.pan();
            h.step(pan);
            h.advance(Duration::from_millis(5));
            let mut baked = test_picture(start().size);
            (baked.source, baked.carries_blur, baked.taken) =
                (Route::Baked, true, h.at() - Duration::from_millis(30));
            let frame_zero = Event::Pictures(crate::pictures::event::Event::Delivered {
                tag: Tag {
                    client: CLIENT,
                    generation: h.generation(),
                },
                why: Why::FrameZero,
                sent: h.at(),
                pictures: vec![(wid(1), baked, start())],
            });
            let shown = only_batch(&h.step(frame_zero)).seq;
            h.commit(shown);
            h.advance(Duration::from_millis(50));
            let swapped = only_batch(&h.step(background(h.generation(), h.at()))).seq;
            h.commit(swapped);
            h.advance(Duration::from_millis(400));
            let lift = lift_batch(&h.step(Event::Tick)).expect("lifted").seq;
            h.commit(lift);
            h.step(Event::Quiet(Generation(h.generation())));
            let burst = bursts.try_recv().expect("the settled burst");
            let worn: Vec<_> = burst
                .marks
                .iter()
                .filter_map(|stamped| match stamped.mark {
                    Mark::Wears {
                        window,
                        at,
                        route,
                        blur,
                        taken_before_us,
                    } => Some((window, at, route, blur, taken_before_us)),
                    _ => None,
                })
                .collect();
            assert_eq!(worn.len(), 3, "{worn:?}");
            assert_eq!(
                (worn[0].0, worn[0].1, worn[0].2, worn[0].3),
                (wid(1), Wearing::Compose, Route::ScreenCaptureKit, false),
                "the cache's"
            );
            assert_eq!(worn[1], (wid(1), Wearing::FrameZero, Route::Baked, true, 30_000));
            assert_eq!(
                (worn[2].1, worn[2].2, worn[2].3),
                (Wearing::Swap, Route::ScreenCaptureKit, false)
            );
        }

        #[test]
        fn the_engine_sends_the_writer_each_burst_journalled_gives() {
            let mut h = Harness::new();
            let bursts = h.writing();
            let lift = lift_batch(&h.lifted()).expect("lifted").seq;
            h.commit(lift);
            assert!(
                matches!(bursts.try_recv(), Err(TryRecvError::Empty)),
                "nothing while it settles"
            );
            h.step(Event::Quiet(Generation(h.generation())));
            let burst = bursts.try_recv().expect("the settled burst");
            assert!(!burst.marks.is_empty());
            assert!(matches!(bursts.try_recv(), Err(TryRecvError::Empty)), "once");

            h.step(Event::Journal(JournalSettings {
                write: false,
                measure_handover: false,
            }));
            let lift = lift_batch(&h.lifted()).expect("lifted").seq;
            h.commit(lift);
            h.step(Event::Quiet(Generation(h.generation())));
            assert!(
                matches!(bursts.try_recv(), Err(TryRecvError::Disconnected)),
                "the writer let go, nothing sent"
            );
        }
    }
}
