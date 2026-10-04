//! The flight engine: the actor that carries out every transition. It holds the lifecycle with its
//! journal (`crate::transitions::domain::journal`'s `Journalled`, over
//! `crate::transitions::domain::lifecycle`'s `Coordinator`), stages each request and merges each
//! pass into the flight running, and does what the coordinator says: it hands the renderer
//! (`crate::animation::platform::overlay`) the edits of each flight's score
//! (`crate::transitions::domain::flight_score`) and asks and watches the pictures service
//! (`crate::pictures::platform::service`), and sends each burst of the journal to its writer
//! (`crate::transitions::platform::journal`). When anything happens is the coordinator's. Runs on the
//! main thread because Core Animation requires it.
//!
//! Design in `src/transitions/docs/coordination.md`; how a flight moves in
//! `src/animation/docs/animation-smoothness.md`; measurements in `src/pictures/docs/capture-research.md` and
//! `src/animation/docs/capture-overlay-research.md`.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use objc2_foundation::MainThreadMarker;
use rini_core::ids::{WindowId, WindowServerId};
use rini_geometry::space::{
    FromCg, GroupRect, Overlay, OverlayPoint, OverlayRect, OverlayVector, Point, Respace,
    ScreenRect, ScreenVector, Size, SurfacePoint, TileSize, ToCg,
};
use rini_geometry::{SameAs, from_overlay_space, to_overlay_space};
use rini_runloop::channel;
use rini_runloop::run_loop::RepeatingTimer;
use tracing::{debug, warn};

use crate::animation::domain::choreography::{Cue, Look, Pass, Stage};
use crate::animation::domain::motion::accents::Accent;
use crate::animation::domain::motion::hairline::corner_radius;
use crate::animation::domain::motion::plan::{self, entrance_from};
use crate::animation::domain::motion::score::{Edit, Node, Value};
use crate::animation::domain::motion::surface::{SurfaceGeometry, pan_travel};
use crate::animation::platform::overlay::{OverlayTile, TileOverlay, media_now};
use crate::pictures::domain::knowledge::{Call, Knowledge};
use crate::pictures::domain::schedule::{Ask, Display, Hold, MAX_SYNC_ENTRANCE_CAPTURES, Priority};
use crate::pictures::domain::stand_in::picture_or_stand_in;
use crate::pictures::domain::target::{PictureTarget, synthetic_window_id};
use crate::pictures::domain::watch::{
    CHANGE_EVERY, CHANGE_TRIES, Client, END_SLACK, LIVE_EVERY, Moment, OnScreen, Settle, Tag, Watch,
    WatchId, Why,
};
use crate::pictures::platform::picture::{Picture, Route};
use crate::pictures::platform::service::Pictures;
use crate::pictures::platform::skylight::capture_via_framed;
use crate::transitions::domain::admission::*;
use crate::transitions::domain::flight::*;
use crate::transitions::domain::flight_score::FlightScore;
use crate::transitions::domain::journal::Journalled;
use crate::transitions::domain::lifecycle::{
    self, Command, EndWatch, Generation, Kind, Start, StageSummary, TileSummary, drawn_focused,
    extend_latest, losing_focus,
};
use crate::transitions::domain::request::{AnimationRequest, Cause, SurfaceWindow};
use crate::transitions::domain::timing::*;
use crate::transitions::domain::travel::{TilePath, is_moving, tile_path};
use crate::transitions::platform::journal::Writer;
use crate::transitions::settings::JournalSettings;

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
    },
    /// Slide every currently visible window in from an offset, purely to evaluate animation quality
    /// by eye. Does not touch any real window, so it is safe to fire at any time.
    DebugSlide {
        dx: f64,
        dy: f64,
        duration: Duration,
        cause: Cause,
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
    /// The key window changed, which turned these windows' looks, each at the frame it is at. Sent by
    /// the reactor for every change it hears of, rini's own raises' included.
    LooksTurned(Vec<(WindowId, ScreenRect)>),
    /// What the pictures service landed. Posted by its sink.
    Pictures(crate::pictures::event::Event),
    /// Whether the journal is written. Sent by the reactor at start and at each config reload.
    Journal(JournalSettings),
}

/// Called with real-window frames to apply while the overlay covers them.
pub type PlaceFrames = Box<dyn Fn(Vec<(WindowId, ScreenRect)>)>;

/// Called with `true` when a transition is reported up and `false` once it has settled, the quiet
/// after its lift; the coordinator's `ReportUp` and `ReportSettled`.
pub type OnFlight = Box<dyn Fn(bool)>;

pub type Sender = channel::Sender<Event>;
pub type Receiver = channel::Receiver<Event>;

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
    /// The window drawn focused until focus hands over: the one focused as the flight began.
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
}

/// What a swapped-in picture became.
enum Took {
    /// A reserved entrance's tile, joining the flight.
    Entrance(OverlayTile),
    /// The picture on the window's tile, which ends at this size.
    Tile { size: TileSize },
    /// Nothing draws the window.
    Nothing,
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
            plan::Member::Changing { to, .. } | plan::Member::Entrance { to, .. } => {
                plan::overlay_of(to, plan.position_of(plan::GroupKey::Loose))
            }
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
    real: ScreenRect,
    border: (WindowId, ScreenRect),
    snapshot: Picture,
) -> OverlayTile {
    let (window, frame) = border;
    let offset = (frame.origin - real.origin).respace::<Overlay>();
    let follow = |rect: OverlayRect| OverlayRect::new(rect.origin + offset, frame.size.respace());
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

/// What a pass says of itself beside its tiles: its kind, how it starts and runs, and which of its
/// windows are not painted where they end yet.
#[derive(Clone, Debug)]
struct Shape {
    kind: Kind,
    start: Start,
    duration: Duration,
    /// Held for and watched until they repaint: grows, stand-ins and reserved entrances.
    awaiting: Vec<WindowId>,
    /// Watched until they repaint and never held for: windows flying from where they opened.
    repainting: Vec<WindowId>,
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
fn summary(
    tiles: Vec<TileSummary>,
    final_frames: Vec<(WindowId, ScreenRect)>,
    focus: Option<WindowId>,
    losing_focus: Option<WindowId>,
    focused_look: Option<WindowId>,
    key: Option<WindowId>,
    shape: Shape,
    display: ScreenRect,
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
        awaiting: shape.awaiting,
        repainting: shape.repainting,
        start: shape.start,
        duration: shape.duration,
        display,
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
            companion: t.companion.is_some(),
        });
        let entrances = self.entrances.iter().map(|e| TileSummary {
            window: e.window,
            start: from_overlay_space(entrance_from(e.to), display),
            end: from_overlay_space(e.to, display),
            floating: e.floating,
            companion: false,
        });
        tiles.chain(entrances).collect()
    }

    /// The flight as composed, for the coordinator.
    fn stage(&self, shape: Shape, display: ScreenRect, opaque: &dyn Fn(WindowId) -> bool) -> StageSummary {
        summary(
            self.tile_summaries(display),
            self.final_frames.clone(),
            self.focus,
            losing_focus(self.focus, self.shown_focus),
            self.focused_look,
            self.key,
            shape,
            display,
            opaque,
        )
    }

    /// Frame zero's pictures, each on its window's tile; how many were taken.
    fn wear(&mut self, pictures: Vec<(WindowId, Picture)>) -> usize {
        let mut worn = 0;
        for (window, picture) in pictures {
            if let Some(tile) = self.tiles.iter_mut().find(|t| t.window == window && t.companion.is_none()) {
                tile.snapshot = picture;
                worn += 1;
            }
        }
        worn
    }

    /// The tile a reserved entrance becomes with its first picture, banded with the rest; `None`
    /// for a window not waiting to enter.
    fn admit(&mut self, window: WindowId, picture: &Picture) -> Option<OverlayTile> {
        let at = self.entrances.iter().position(|e| e.window == window)?;
        let entrance = self.entrances.remove(at);
        self.merge(entrance_tile(&entrance, picture));
        restack(&mut self.tiles, self.focus);
        self.tiles.iter().find(|t| t.window == window).cloned()
    }

    /// A picture swapped in for `window`: a reserved entrance joins with it, else its tile takes it in
    /// place, a stand-in's included, moving on as it was and flying no focused look from now.
    fn take_picture(&mut self, window: WindowId, picture: &Picture) -> Took {
        if let Some(tile) = self.admit(window, picture) {
            return Took::Entrance(tile);
        }
        let Some(tile) = self.tiles.iter_mut().find(|t| t.window == window && t.companion.is_none()) else {
            return Took::Nothing;
        };
        tile.snapshot = picture.clone();
        if self.focused_look == Some(window) {
            self.focused_look = None;
        }
        Took::Tile { size: tile.to.size.respace() }
    }

    /// `target`'s tile flies `look`, drawn focused, if it fits where the tile starts. Whether it does.
    fn fly_focused_look(&mut self, target: WindowId, look: Option<Picture>) -> bool {
        let Some(tile) = self.tiles.iter_mut().find(|t| t.window == target) else { return false };
        let Some(look) = look.filter(|look| look.fits(tile.from.size)) else {
            return false;
        };
        tile.snapshot = look;
        tile.focused = true;
        self.focused_look = Some(target);
        true
    }

    /// Focus handed over: `to` is drawn focused from now, `from` not.
    fn hand_over_focus(&mut self, to: WindowId, from: Option<WindowId>) {
        self.shown_focus = Some(to);
        for tile in self.tiles.iter_mut() {
            if tile.window == to {
                tile.focused = true;
            } else if Some(tile.window) == from {
                tile.focused = false;
            }
        }
    }

    /// Folds one later pass into the flight's tiles and focus; the overlay follows `merge_plans`. A
    /// pass moving focus redraws which tiles are drawn focused, and none flies a focused look until
    /// the engine gives the new target its own.
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
            for tile in self.tiles.iter_mut().filter(|t| t.companion.is_none()) {
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
            let merged = self.score.merge(&self.plan, pass, at);
            self.plan = merged.plan;
            sync_tiles_to_plan(&mut self.tiles, &self.plan);
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
                // The original start is kept so a moving window is not yanked backwards, and the
                // picture and focus it is drawn with: a pass is composed from the cache, and a tile
                // changes its picture only for one the flight took.
                existing.to = tile.to;
                existing.floating = tile.floating;
                existing.server_order = tile.server_order;
                existing.depth = tile.depth;
                existing.companion = tile.companion;
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
    windows.iter().map(|&(window, frame)| (target_at(window, frame), frame)).collect()
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

/// Windows whose look turned with the key window, at background priority: no warm filter drops it
/// for a picture that fits, and it waits for any transition's release.
fn refresh(windows: Vec<(WindowId, ScreenRect)>) -> Ask {
    Ask {
        priority: Priority::Background,
        targets: sized_targets(windows),
    }
}

/// The radius a tile of `size` clips its picture to: a SkyLight picture of a window baked with the
/// desktop behind it has the wallpaper in its corners. `None` draws it whole.
fn corner_clip(knowledge: Knowledge, picture: &Picture, size: TileSize) -> Option<f64> {
    (picture.source == Route::SkyLight && knowledge.after_alpha() == Call::Baked)
        .then(|| corner_radius(knowledge.radius, size))
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
                behind: behind.clone(),
                every: CHANGE_EVERY,
                tries: CHANGE_TRIES,
            }),
        ];
        for (slot, watch) in [&mut self.landing, &mut self.awaiting, &mut self.change].into_iter().zip(watches) {
            put(slot, watch, pictures);
        }
        let live = (!end.background.is_empty()).then(|| Watch::Live {
            tag,
            windows: end.background.iter().map(|&(window, frame)| target_at(window, frame)).collect(),
            every: LIVE_EVERY,
            after: self.change,
            initial: end.background_initial,
        });
        put(&mut self.live, live, pictures);
    }

    fn stop(&mut self, pictures: &Pictures) {
        for slot in [&mut self.landing, &mut self.awaiting, &mut self.change, &mut self.live] {
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

pub struct FlightEngine {
    rx: Receiver,
    /// Timers post back into this actor's own queue through it.
    tx: Sender,
    mtm: MainThreadMarker,
    overlay: Option<TileOverlay>,
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
    /// When everything happens, and the journal of it.
    flights: Journalled,
    /// What the reactor last said of the journal; `None` until it has.
    journal: Option<JournalSettings>,
    /// Writes each burst `flights` gives while the journal is written.
    writer: Option<Writer>,
    running: Option<RunningAnimation>,
    /// The latest picture delivered of each window, under its generation, until it is swapped in: the
    /// coordinator holds a landing back from over a focused look until focus hands over.
    delivered: HashMap<WindowId, (Generation, Picture)>,
    watches: EndWatches,
    /// The frame clock, from a transition's begin to its lift.
    clock: Option<RepeatingTimer>,
    /// The quiet after the lift of a generation.
    quiet: Option<(Generation, RepeatingTimer)>,
    /// Places the real windows once the overlay covers them. Supplied by the owner.
    place_frames: Option<PlaceFrames>,
    on_flight: Option<OnFlight>,
}

impl FlightEngine {
    /// `pictures` tells this engine what lands through its sink, as `Event::Pictures`; `client` is
    /// the engine as the application numbers the service's clients.
    pub fn new(
        rx: Receiver,
        tx: Sender,
        mtm: MainThreadMarker,
        pictures: Pictures,
        client: Client,
    ) -> Self {
        Self {
            rx,
            tx,
            mtm,
            overlay: None,
            pictures,
            client,
            display: None,
            display_id: None,
            top_band: 0.0,
            flights: Journalled::default(),
            journal: None,
            writer: None,
            running: None,
            delivered: HashMap::new(),
            watches: EndWatches::default(),
            clock: None,
            quiet: None,
            place_frames: None,
            on_flight: None,
        }
    }

    pub fn set_place_frames(&mut self, place: PlaceFrames) {
        self.place_frames = Some(place);
    }

    pub fn set_on_flight(&mut self, on_flight: OnFlight) {
        self.on_flight = Some(on_flight);
    }

    pub async fn run(mut self) {
        while let Some((span, event)) = self.rx.recv().await {
            let _guard = span.enter();
            self.handle(event);
        }
    }

    /// One event. A request's press goes to the journal first, for the request's first drive to mark.
    fn handle(&mut self, event: Event) {
        match event {
            Event::SetDisplay {
                id,
                frame,
                scale,
                top_band,
            } => self.set_display(id, frame, scale, top_band),
            Event::Animate { windows, focus, focused_before, key, duration, look, cause } => {
                self.flights.pressed(cause);
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
                self.flights.pressed(cause);
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
                self.flights.pressed(cause);
                self.start_bounce(windows, overshoot, final_frames, focus, duration, (look, cue))
            }
            Event::DebugSlide { dx, dy, duration, cause } => {
                self.flights.pressed(cause);
                self.debug_slide(dx, dy, duration)
            }
            Event::Tick => self.tick(),
            Event::Quiet(generation) => {
                if self.quiet.as_ref().is_some_and(|(armed, _)| *armed == generation) {
                    self.quiet = None;
                }
                self.drive(lifecycle::Event::Quiet(generation));
            }
            Event::Pictures(landed) => self.pictures_landed(landed),
            Event::LooksTurned(windows) => self.drive(lifecycle::Event::LooksTurned(windows)),
            Event::Journal(settings) => self.set_journal(settings),
        }
        self.flights.answered();
    }

    /// Tells the coordinator `event`, carries out what it says, and sends the writer the burst due.
    fn drive(&mut self, event: lifecycle::Event) {
        let now = Instant::now();
        let (commands, burst) = self.flights.drive(event, now);
        for command in commands {
            self.carry_out(command, now);
        }
        if let (Some(burst), Some(writer)) = (burst, &self.writer) {
            writer.send(burst);
        }
    }

    /// The journal written or not, as the reactor says. Turned on, what the ring holds is written at
    /// once, or at the settle of the burst up.
    fn set_journal(&mut self, settings: JournalSettings) {
        if self.journal == Some(settings) {
            return;
        }
        if !settings.write {
            self.flights.set_writing(false);
            self.writer = None;
        } else {
            let Some(writer) = Writer::start(rini_core::paths::journal_file()) else { return };
            if let Some(burst) = self.flights.set_writing(true) {
                writer.send(burst);
            }
            self.writer = Some(writer);
        }
        self.journal = Some(settings);
    }

    fn carry_out(&mut self, command: Command, now: Instant) {
        match command {
            Command::Hold => self.pictures.hold(Hold::Transition),
            Command::Settle => self.pictures.hold(Hold::Settling),
            Command::Release => self.pictures.hold(Hold::Released),
            Command::AskFrameZero { generation, strip, background, behind } => self
                .pictures
                .ask_on_screen(on_screen(self.tag(generation), Moment::FrameZero, &strip, &background, &behind)),
            Command::Show => {
                if let Some(overlay) = self.overlay.as_mut() {
                    overlay.show();
                }
            }
            Command::Write(frames) => self.request_frames(frames),
            Command::Holes(ends) => {
                if let Some(overlay) = self.overlay.as_mut() {
                    overlay.set_holes(&ends);
                }
            }
            Command::Watch(end) => self.watches.watch(&self.pictures, self.client, &end),
            Command::StopWatches => self.watches.stop(&self.pictures),
            Command::StartMoving => self.start_moving(now),
            Command::AskFinal { generation, windows, behind } => self
                .pictures
                .ask_on_screen(on_screen(self.tag(generation), Moment::Final, &windows, &[], &behind)),
            Command::Swap { generation, window } => self.swap(generation, window, now),
            Command::Snap { window, frame } => self.snap(window, frame),
            Command::KeepFocusedLook { generation, window } => {
                self.pictures.keep_focused(self.tag(generation), window)
            }
            Command::HandOverFocus { to, from } => {
                if let Some(running) = self.running.as_mut() {
                    running.hand_over_focus(to, from);
                }
                if let Some(overlay) = self.overlay.as_mut() {
                    overlay.set_tile_focused(to, true);
                    if let Some(from) = from {
                        overlay.set_tile_focused(from, false);
                    }
                }
            }
            Command::Lift { generation } => self.lift(generation),
            Command::Warm(windows) => {
                if !windows.is_empty() {
                    self.pictures.ask(Ask {
                        priority: Priority::Warm,
                        targets: sized_targets(windows),
                    });
                }
            }
            Command::Refresh(windows) => self.pictures.ask(refresh(windows)),
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

    fn report(&self, flying: bool) {
        if let Some(on_flight) = &self.on_flight {
            on_flight(flying);
        }
    }

    /// What the pictures service landed: the deliveries and the lost landings under this engine's tag
    /// go to the coordinator, each delivery journalled as it came and frame zero's onto their tiles
    /// first. Any other capture is the cache's alone.
    fn pictures_landed(&mut self, landed: crate::pictures::event::Event) {
        match landed {
            crate::pictures::event::Event::Taken { .. } => {}
            crate::pictures::event::Event::Delivered { tag, why, sent, pictures } => {
                let Some(generation) = Generation::for_client(tag, self.client) else { return };
                let taken = pictures.iter().map(|(window, picture, frame)| (*window, *frame, picture.taken));
                self.flights.delivery(Instant::now(), generation, why, sent, taken);
                if why == Why::FrameZero {
                    self.frame_zero(generation, pictures);
                    return;
                }
                self.offer(generation, why, pictures);
            }
            crate::pictures::event::Event::Lost { tag, window } => {
                let Some(generation) = Generation::for_client(tag, self.client) else { return };
                debug!(pid = window.pid, idx = window.idx.get(), generation = generation.0, "end window's picture will not come");
                self.drive(lifecycle::Event::Lost { generation, window });
            }
        }
    }

    /// Each delivered picture to the coordinator, which says whether it is swapped in.
    fn offer(&mut self, generation: Generation, why: Why, pictures: Vec<(WindowId, Picture, ScreenRect)>) {
        for (window, picture, frame) in pictures {
            self.delivered.insert(window, (generation, picture));
            self.drive(lifecycle::Event::Picture { generation, window, frame, why });
        }
    }

    /// Frame zero's pictures, put on their tiles while the overlay is not up yet, then the overlay can
    /// go up; once it is up without them, each is offered to its tile as a swap, and frame zero is
    /// never installed again under a moving flight.
    fn frame_zero(&mut self, generation: Generation, pictures: Vec<(WindowId, Picture, ScreenRect)>) {
        if self.flights.coordinator().is_shown() {
            self.offer(generation, Why::FrameZero, pictures);
        } else if self.flights.coordinator().generation() == generation
            && self.flights.coordinator().is_flying()
            && let Some(running) = self.running.as_mut()
            && running.wear(pictures.into_iter().map(|(window, picture, _)| (window, picture)).collect()) > 0
        {
            self.reinstall();
        }
        self.drive(lifecycle::Event::FrameZero(generation));
    }

    /// The picture delivered of `window` under `generation`, onto its tile; a reserved entrance's
    /// first joins the flight with it.
    fn swap(&mut self, generation: Generation, window: WindowId, now: Instant) {
        if self.delivered.get(&window).is_none_or(|(delivered, _)| *delivered != generation) {
            return;
        }
        let Some((_, picture)) = self.delivered.remove(&window) else { return };
        let Some(running) = self.running.as_mut() else { return };
        let size = match running.take_picture(window, &picture) {
            Took::Entrance(tile) => return self.admit_entrance(tile, now),
            Took::Tile { size } => size,
            Took::Nothing => return,
        };
        debug!(pid = window.pid, idx = window.idx.get(), generation = generation.0, flown = running.flown, "picture swapped mid-flight");
        if !running.flown {
            self.reinstall();
            return;
        }
        let clip = corner_clip(self.pictures.knowledge(WindowServerId::from(window)), &picture, size);
        if let Some(overlay) = self.overlay.as_mut() {
            overlay.swap(window, &picture);
            overlay.set_tile_corner_clip(window, clip);
        }
    }

    /// The coordinator's snap of `window`'s tile onto `frame`, its window's, as an edit of the score
    /// played on the renderer.
    fn snap(&mut self, window: WindowId, frame: OverlayRect) {
        let Self { overlay, running, .. } = self;
        let (Some(overlay), Some(running)) = (overlay.as_mut(), running.as_mut()) else {
            return;
        };
        let edits = running.score.snap(window, frame, media_now());
        overlay.play(&edits, &running.tiles);
    }

    /// A reserved entrance's tile, with its first picture: it joins frame zero, or the flight for what
    /// is left of it, merged in as a pass holding it alone.
    fn admit_entrance(&mut self, tile: OverlayTile, now: Instant) {
        let left = self.flights.coordinator().remaining(now).unwrap_or_default();
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
            let (from, to) = (plan::group_relative(tile.from, loose), plan::group_relative(tile.to, loose));
            running.plan.entrances.push((tile.window, from, to));
            self.reinstall();
            return;
        }
        let mut entrance = plan::ReflowPlan::empty();
        entrance.entrances.push((tile.window, plan::installed(tile.from), plan::installed(tile.to)));
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
        let merged = running.score.merge(&running.plan, &pass, media_now());
        running.plan = merged.plan;
        let banding = band_plan(&running.plan, &running.tiles, running.focus);
        let mut edits = merged.edits;
        edits.push(running.score.bank(banding));
        if let Some(overlay) = self.overlay.as_mut() {
            overlay.play(&edits, &running.tiles);
        }
    }

    /// The display the overlay covers: what a member's destination is judged against.
    fn viewport(&self) -> OverlayRect {
        self.display
            .map(|(display, _)| to_overlay_space(display, display))
            .unwrap_or(OverlayRect::new(Point::new(-1e9, -1e9), Size::new(2e9, 2e9)))
    }

    /// The overlay's geometry. The pictures of the display are the service's, told by the reactor.
    fn set_display(&mut self, id: u32, frame: ScreenRect, scale: f64, top_band: f64) {
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

    /// Frame zero posed again, its plan as it is, with the pictures the tiles hold now; the accents
    /// waiting for the motion keep waiting.
    fn reinstall(&mut self) {
        let Self { overlay, running, .. } = self;
        let (Some(overlay), Some(running)) = (overlay.as_mut(), running.as_mut()) else {
            return;
        };
        let banding = band_plan(&running.plan, &running.tiles, running.focus);
        let drawn = running.drawn();
        let stage = Stage {
            plan: &running.plan,
            drawn: &drawn,
            seconds: 0.0,
            cue: running.cue,
            accents: &[],
        };
        overlay.pose(running.score.pose(&stage, banding), &running.tiles);
        self.clip_corners();
    }

    /// Every tile whose picture is baked with the desktop behind it clipped to its corners; every
    /// install draws a picture whole.
    fn clip_corners(&mut self) {
        let Some(running) = self.running.as_ref() else { return };
        let clips: Vec<(WindowId, f64)> = running
            .tiles
            .iter()
            .filter_map(|tile| {
                let knowledge = self.pictures.knowledge(WindowServerId::from(tile.window));
                Some((tile.window, corner_clip(knowledge, &tile.snapshot, tile.to.size.respace())?))
            })
            .collect();
        if let Some(overlay) = self.overlay.as_mut() {
            for (window, radius) in clips {
                overlay.set_tile_corner_clip(window, Some(radius));
            }
        }
    }

    /// Tiles for the border windows tracing the animated windows; each anchor is the window's real
    /// frame and its tile. See "Border companions" in `src/transitions/docs/coordination.md`.
    fn companion_tiles(
        &mut self,
        display: ScreenRect,
        anchors: &[(ScreenRect, &OverlayTile)],
        exclude: &std::collections::HashSet<u32>,
        needs_capture: &mut Vec<PictureTarget>,
    ) -> Vec<OverlayTile> {
        if anchors.is_empty() {
            return Vec::new();
        }
        // Every window rini manages is excluded, not only the pass's: parked windows share a frame.
        let managed = managed_server_ids(exclude, self.pictures.windows().into_iter());
        let candidates: Vec<(WindowServerId, ScreenRect)> =
            crate::windows::platform::window_server::visible_windows_on_display(display.to_cg())
                .into_iter()
                .filter(|(id, _)| !managed.contains(&id.as_u32()))
                .map(|(id, frame)| (id, ScreenRect::from_cg(frame)))
                .collect();
        let mut claimed: std::collections::HashSet<u32> = std::collections::HashSet::new();
        let mut tiles = Vec::new();
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
            match self.pictures.usable(window) {
                Some(snapshot) => tiles.push(border_tile(anchor, real, (window, frame), snapshot)),
                None => needs_capture.push(PictureTarget {
                    window,
                    server_id,
                    size: frame.size,
                }),
            }
        }
        tiles
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
            display,
            &|window| self.pictures.knowledge(WindowServerId::from(window)).is_opaque(),
        );
        self.quiet = None;
        let tx = self.tx.clone();
        self.clock = RepeatingTimer::every(FRAME_INTERVAL, move || {
            _ = tx.send(Event::Tick);
        });
        self.drive(lifecycle::Event::Begin(stage));
        if self.clock.is_none() {
            warn!("could not start the frame clock; placing the windows directly");
            self.abandon();
            return false;
        }
        true
    }

    /// The transition could not be staged or shown: its windows are placed directly and nothing
    /// holds the pictures service.
    fn abandon(&mut self) {
        if let Some(overlay) = self.overlay.as_mut() {
            overlay.hide();
            overlay.release_tiles();
        }
        self.running = None;
        self.clock = None;
        self.delivered.clear();
        self.drive(lifecycle::Event::Abandon);
    }

    /// What the flight composed, told to the coordinator.
    fn stage(&self, shape: Shape) -> Option<StageSummary> {
        let (running, (display, _)) = (self.running.as_ref()?, self.display?);
        Some(running.stage(shape, display, &|window| {
            self.pictures.knowledge(WindowServerId::from(window)).is_opaque()
        }))
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
            awaiting: Vec::new(),
            repainting: Vec::new(),
        };
        if !self.flights.coordinator().is_flying() {
            let requested = windows
                .iter()
                .map(|request| TileSummary {
                    window: request.window,
                    start: request.from,
                    end: request.to,
                    floating: request.floating,
                    companion: false,
                })
                .collect();
            if !self.begin(requested, final_frames.clone(), (focus, focused_before), key, shape.clone()) {
                return;
            }
        }
        let (drawing_focus, losing) = self.drawing_focus((focus, focused_before));

        let depths = crate::windows::platform::window_server::front_to_back_depths();

        let mut tiles = Vec::with_capacity(windows.len());
        let mut skipped = 0usize;
        let mut offscreen = 0usize;
        let mut needs_capture: Vec<PictureTarget> = Vec::new();
        let mut entrances: Vec<PendingEntrance> = Vec::new();
        let mut awaiting: Vec<WindowId> = Vec::new();
        let mut repainting: Vec<WindowId> = Vec::new();
        let mut waits = |window, end: EndPicture| match end {
            EndPicture::Awaited => awaiting.push(window),
            EndPicture::Repainting => repainting.push(window),
            EndPicture::Lands => {}
        };
        // Newly opened windows travelling from their spawn frame.
        let mut spawn_entrances: Vec<(WindowId, GroupRect, GroupRect)> = Vec::new();
        let mut sync_captures = 0usize;
        let scale = self.display.map(|(_, scale)| scale).unwrap_or(2.0);
        let mut starts: Vec<(WindowId, ScreenRect)> = Vec::new();
        let mut resolved: Vec<(WindowId, ScreenRect, ScreenRect, bool)> = Vec::new();
        // For `neighbour_travel`: a window leaving for or returning from a park rides its
        // nearest strip neighbour's vector.
        let others: Vec<(ScreenRect, ScreenRect, bool)> =
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
            // leaving a hole; see `stands_in`. It covers nothing, so the flight holds for it like
            // any grow, and its picture replaces the stand-in once it has repainted where it ends.
            let snapshot = picture_or_stand_in(
                snapshot,
                || {
                    crate::windows::platform::window_server::get_window(request.server_id)
                        .map(|info| ScreenRect::from_cg(info.frame))
                },
                display_frame,
                || self.pictures.stand_in(request.to.size, scale),
            );
            match snapshot {
                Some(snapshot) => {
                    waits(request.window, EndPicture::of_picture(snapshot.coverage.covered, request.to.size));
                    tiles.push(OverlayTile {
                        window: request.window,
                        from: to_overlay_space(start, display_frame),
                        to: to_overlay_space(end, display_frame),
                        snapshot,
                        floating: request.floating,
                        server_order: depths.get(&request.server_id.as_u32()).copied(),
                        depth: 0,
                        companion: None,
                        focused: drawn_focused(request.window, drawing_focus, losing),
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
                            .map(|info| ScreenRect::from_cg(info.frame))
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
                    let entrance = entrance_plan(spawn, request.to, usable, budget_left);
                    waits(request.window, EndPicture::of_entrance(entrance));
                    match entrance {
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
                            // Already at its slot (a cold cache, not an open): a still tile.
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
                                focused: !standing
                                    || drawn_focused(request.window, drawing_focus, losing),
                                radius: self.pictures.knowledge(request.server_id).radius,
                            });
                            if standing {
                                starts.push((request.window, from));
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
                            });
                        }
                    }
                }
            }
        }
        // Stacked here so the companions can anchor to their windows' depths.
        restack(&mut tiles, drawing_focus);
        let anchors: Vec<(ScreenRect, &OverlayTile)> = starts
            .iter()
            .filter_map(|(window, start)| {
                Some((*start, tiles.iter().find(|tile| tile.window == *window)?))
            })
            .collect();
        let exclude: std::collections::HashSet<u32> =
            windows.iter().map(|request| request.server_id.as_u32()).collect();
        let companions =
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

        let moving_drawable = tiles.iter().any(|tile| is_moving(tile.from, tile.to));
        if !worth_flying(moving_drawable, self.running.is_some()) {
            self.abandon();
            return;
        }

        let mut plan = plan::reflow_plan(&resolved, display_frame);
        plan.entrances.extend(spawn_entrances);
        for tile in &tiles {
            if plan.member(tile.window).is_none() {
                plan.adopt(tile);
            }
        }

        self.begin_group(Composed {
            tiles,
            final_frames,
            label: "per-window",
            shape: Shape {
                awaiting,
                repainting,
                ..shape
            },
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
        let backdrop_drawn = self.overlay.as_ref().is_some_and(TileOverlay::backdrop_drawn);
        let dressing = self
            .picture_display()
            .map(|display| self.pictures.dressing(display, backdrop_drawn));
        let display = self.display.map(|(frame, _)| frame);
        let Some(overlay) = self.ensure_overlay() else {
            self.abandon();
            return;
        };
        if let (Some(dressing), Some(display)) = (&dressing, display) {
            let strip = dressing.strip.map(|strip| to_overlay_space(strip, display));
            log_dressing(pass.label, dressing, strip);
            overlay.set_backdrop(dressing.desktop.as_ref());
            overlay.set_bar(dressing.bar.as_ref(), strip);
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
        };
        if let Some(accent) = pass.accent {
            running.score.wait(accent);
        }
        restack(&mut running.tiles, focus);
        self.running = Some(running);
        let Some(mut stage) = self.stage(pass.shape) else { return };
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
        self.running.as_mut().is_some_and(|running| running.fly_focused_look(target, look))
    }

    /// On a flight that moves, the tiles a merge turned focused or not drawn so, and the focused look
    /// a new parked target flies cut onto its tile: a retarget draws neither.
    fn redraw_focus(&mut self, before: &[(WindowId, bool)], look: Option<WindowId>) {
        let (Some(running), Some(overlay)) = (self.running.as_ref(), self.overlay.as_mut()) else { return };
        for tile in running.tiles.iter().filter(|t| t.companion.is_none()) {
            let was = before.iter().find(|(w, _)| *w == tile.window).map(|(_, focused)| *focused);
            if look == Some(tile.window) {
                let knowledge = self.pictures.knowledge(WindowServerId::from(tile.window));
                overlay.swap(tile.window, &tile.snapshot);
                let clip = corner_clip(knowledge, &tile.snapshot, tile.to.size.respace());
                overlay.set_tile_corner_clip(tile.window, clip);
                overlay.set_tile_focused(tile.window, true);
            } else if was.is_some_and(|was| was != tile.focused) {
                overlay.set_tile_focused(tile.window, tile.focused);
            }
        }
    }

    /// One later pass folded into the flight running: bent by its look from the score once the plan
    /// plays, folded into frame zero and posed again before. A pass moving focus to a target parked
    /// off the display flies its focused look, as the first pass's does.
    fn merge_group(&mut self, pass: Composed) {
        let at = media_now();
        let left = self.flights.coordinator().remaining(Instant::now()).unwrap_or_default();
        let viewport = self.viewport();
        let display = self.display.map(|(frame, _)| frame);
        let Self { overlay, running, .. } = self;
        let Some(running) = running.as_mut() else { return };
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
        let drawn_focused_before: Vec<(WindowId, bool)> =
            running.tiles.iter().map(|t| (t.window, t.focused)).collect();
        let (pass_focus, _) = pass.focus;
        running.merge_pass(pass.tiles, pass_focus);
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
                        presented = format!("{:.0},{:.0}", drawn.x, drawn.y),
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
                        | plan::Member::Entrance { to, .. } => plan::overlay_of(
                            to,
                            running.plan.position_of(plan::GroupKey::Loose),
                        ),
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
            moved = delta.moves_anything();
            let banding = band_plan(&running.plan, &running.tiles, running.focus);
            edits.push(running.score.bank(banding));
            if let Some(overlay) = overlay.as_mut() {
                overlay.play(&edits, &running.tiles);
            }
            // A merge that moves anything restarts the clock, over everything the score plays.
            shape.duration = running.score.clock(shape.duration, at);
        }
        let flown = running.flown;
        let extend = (flown && !moved).then(|| running.score.outlasting(left, at)).flatten();
        let focus_moved = running.focus != focus_before;
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
            awaiting: Vec::new(),
            repainting: Vec::new(),
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
                        companion: false,
                    }
                })
                .collect();
            if !self.begin(requested, final_frames.clone(), focus, key, shape.clone()) {
                return;
            }
        }
        let (drawing_focus, losing) = self.drawing_focus(focus);

        let depths = crate::windows::platform::window_server::front_to_back_depths();
        // One query for every window, not one each: a round trip per window stalled this thread
        // for 100-200ms a press. See "Rapid presses" in `specs/focus.md`.
        let real_frames: HashMap<WindowServerId, ScreenRect> =
            crate::windows::platform::window_server::get_windows(
                &windows.iter().map(|w| w.server_id).collect::<Vec<_>>(),
            )
            .into_iter()
            .map(|info| (info.id, ScreenRect::from_cg(info.frame)))
            .collect();
        let mut tiles = Vec::with_capacity(windows.len());
        let mut missing = 0usize;
        let mut misshapen = 0usize;
        let mut stand_ins = 0usize;
        let mut needs_capture: Vec<PictureTarget> = Vec::new();
        let mut starts: Vec<(WindowId, ScreenRect)> = Vec::new();
        let scale = self.display.map(|(_, scale)| scale).unwrap_or(2.0);
        for window in &windows {
            let (from, to) = window.travel(from_offset, to_offset);
            let cached = self.pictures.usable(window.window);
            // A known window parked off this display with no picture flies as the stand-in, as on
            // the per-window path; see `stands_in`.
            let snapshot = picture_or_stand_in(
                cached,
                || real_frames.get(&window.server_id).copied(),
                display,
                || self.pictures.stand_in(window.frame.size.respace(), scale),
            );
            match snapshot {
                Some(snapshot) => {
                    // A wrong-shaped picture is stretched rather than dropped. See "A window that
                    // was resized keeps a usable picture" in `src/pictures/docs/capture-research.md`.
                    if snapshot.source == Route::StandIn {
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
                        focused: drawn_focused(window.window, drawing_focus, losing),
                        radius: self.pictures.knowledge(window.server_id).radius,
                    });
                }
                // Still placed by `final_frames`, and warmed once the movement settles.
                None => missing += 1,
            }
        }
        restack(&mut tiles, drawing_focus);
        let anchors: Vec<(ScreenRect, &OverlayTile)> = starts
            .iter()
            .filter_map(|(window, real)| {
                Some((*real, tiles.iter().find(|tile| tile.window == *window)?))
            })
            .collect();
        let exclude: std::collections::HashSet<u32> =
            windows.iter().map(|window| window.server_id.as_u32()).collect();
        let companions = self.companion_tiles(display, &anchors, &exclude, &mut needs_capture);
        tiles.extend(companions);
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

        // A strip movement never resizes, never carries a brand-new window and never holds: a stand-in
        // whose window comes on screen is pictured once it lands there.
        self.begin_group(Composed {
            tiles,
            final_frames,
            label: "surface",
            shape,
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
        let Self { overlay, running, .. } = self;
        let Some(running) = running.as_mut() else { return };
        let at = media_now();
        let Some(edits) = running.push(&cue, overshoot, duration.as_secs_f64(), at) else { return };
        if let Some(overlay) = overlay.as_mut() {
            overlay.play(&edits, &running.tiles);
        }
        debug!(
            overshoot = format!("{:.0},{:.0}", overshoot.x, overshoot.y),
            "edge bounce"
        );
        let remaining = running.score.remaining(at);
        self.drive(lifecycle::Event::Extend(remaining));
    }

    /// The motion starts: the look composes it from frame zero, with the accents that waited for it,
    /// and the renderer plays it in one transaction; the clock runs on for as long as the score does.
    fn start_moving(&mut self, now: Instant) {
        let duration = self.flights.coordinator().remaining(now).unwrap_or_default();
        let Self { overlay, running, .. } = self;
        let Some(running) = running.as_mut() else { return };
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
        let at = media_now();
        let drawn = running.drawn();
        let stage = Stage {
            plan: &running.plan,
            drawn: &drawn,
            seconds: duration.as_secs_f64(),
            cue: running.cue,
            accents: &[],
        };
        let edits = running.score.start_moving(&stage, at);
        if let Some(overlay) = overlay.as_mut() {
            overlay.play(&edits, &running.tiles);
        }
        let remaining = running.score.remaining(at);
        self.drive(lifecycle::Event::Extend(remaining));
    }

    /// One frame of the clock: where the tiles are drawn and where the windows are, read only once
    /// the clock is done.
    fn tick(&mut self) {
        if let Some(overlay) = self.overlay.as_ref() {
            overlay.trace(media_now());
        }
        if !self.flights.coordinator().is_flying() {
            return;
        }
        let (settled, landed) = if self.flights.coordinator().wants_landing(Instant::now()) {
            let now = media_now();
            let settled = self.running.as_ref().is_none_or(|running| running.score.settled(now));
            let landed = self.handover().as_ref().is_none_or(|report| report.count_over == 0);
            (settled, landed)
        } else {
            (false, false)
        };
        self.drive(lifecycle::Event::Tick { settled, landed });
    }

    /// Overlay down, over the real windows where they end; the quiet after it begins.
    fn lift(&mut self, generation: Generation) {
        self.report_handover_error();
        if let Some(overlay) = self.overlay.as_mut() {
            overlay.hide();
            // The acceptance greps pair "placing real windows" with this line.
            let windows = self.running.as_ref().map_or(0, |running| running.tiles.len());
            debug!(windows, "overlay lifted");
            overlay.release_tiles();
        }
        self.running = None;
        self.clock = None;
        self.delivered.clear();
        let tx = self.tx.clone();
        match RepeatingTimer::every(SETTLE_BEFORE_CAPTURES, move || {
            _ = tx.send(Event::Quiet(generation));
        }) {
            Some(timer) => self.quiet = Some((generation, timer)),
            None => self.tx.send(Event::Quiet(generation)),
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
        let by_server: HashMap<WindowServerId, ScreenRect> =
            crate::windows::platform::window_server::get_windows(
                &asked.iter().map(|w| WindowServerId::new(w.idx.get())).collect::<Vec<_>>(),
            )
            .into_iter()
            .map(|info| (info.id, ScreenRect::from_cg(info.frame)))
            .collect();
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
        debug!(count = targets.len(), "asking for pictures the flight has none of");
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
        let Some(place) = &self.place_frames else {
            warn!("no frame sink; cannot place windows at their final frames");
            return;
        };
        debug!(count = frames.len(), "placing real windows behind the overlay");
        place(frames);
    }

    /// Slides every window on screen in from an offset; touches no real window.
    fn debug_slide(&mut self, dx: f64, dy: f64, duration: Duration) {
        let Some((display_frame, _)) = self.display else {
            warn!("no display geometry yet; cannot run the debug slide");
            return;
        };
        let windows =
            crate::windows::platform::window_server::visible_windows_on_display(display_frame.to_cg());
        if windows.is_empty() {
            warn!("no visible windows found for the debug slide");
            return;
        }
        let requests: Vec<AnimationRequest> = windows
            .into_iter()
            .map(|(server_id, frame)| {
                let frame = ScreenRect::from_cg(frame);
                AnimationRequest {
                    window: synthetic_window_id(server_id),
                    server_id,
                    // The debug slide knows nothing about the layout; everything is on the strip.
                    floating: false,
                    from: frame.translate(ScreenVector::new(dx, dy)),
                    to: frame,
                }
            })
            .collect();
        debug!(count = requests.len(), dx, dy, "running debug slide");
        self.start(requests, (None, None), None, duration, Look::default());
    }
}

/// The frame the window server reports for a window, if it reports a real one: the reactor's
/// `from` can be the previous pass's destination rather than where the window sits.
///
/// A zero-sized frame is macOS saying it has not laid the window out yet, which is not a position to
/// animate from. The geometry decision is `travel::tile_path`; this is the read it needs.
fn live_frame(server_id: WindowServerId) -> Option<ScreenRect> {
    crate::windows::platform::window_server::get_window(server_id)
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
            .map(|b| format!(
                "{:.0}x{:.0} {:?}",
                b.coverage.covered.width, b.coverage.covered.height, b.source
            ))
            .unwrap_or_else(|| "NONE".to_string()),
        bar = dressing
            .bar
            .as_ref()
            .map(|b| format!("{:.0}x{:.0}", b.coverage.covered.width, b.coverage.covered.height))
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
    use rini_geometry::space::{Rect, Respace, ScreenSize, SurfaceRect};

    use super::*;
    use crate::animation::domain::motion::surface::viewport;
    use crate::animation::domain::motion::testing::{DISPLAY, Gen, RUNS, VIEWPORT, rect};
    use crate::pictures::platform::picture::test_picture;

    /// A frame on `DISPLAY`, which sits at the origin, on the overlay over it.
    fn on(frame: ScreenRect) -> OverlayRect { to_overlay_space(frame, DISPLAY) }

    /// An overlay frame back on `DISPLAY`.
    fn screen(frame: OverlayRect) -> ScreenRect { from_overlay_space(frame, DISPLAY) }

    /// A frame on the strip surface where the viewport at rest shows it.
    fn seen(frame: SurfaceRect) -> OverlayRect { viewport(SurfacePoint::origin()).transform_rect(&frame) }

    fn wid(idx: u32) -> WindowId {
        WindowId::new(7, idx)
    }

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

    /// A SkyLight sliver: 40pt of an `size`-wide window, which `is_usable` rejects.
    fn clipped(size: ScreenSize) -> Picture {
        let mut snapshot = test_picture(size);
        snapshot.coverage.covered = ScreenSize::new(40.0, size.height);
        snapshot
    }

    fn layout(awaiting: Vec<WindowId>) -> Shape {
        Shape {
            kind: Kind::LayoutPass,
            start: Start::Coalesced,
            duration: Duration::from_millis(300),
            awaiting,
            repainting: Vec::new(),
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
                landing: vec![(wid(1), rect(4.0, 32.0, 859.0, 1081.0))],
                awaiting: vec![(wid(2), rect(867.0, 32.0, 859.0, 1081.0))],
                landing_until: now + Duration::from_millis(150),
                awaiting_until: now + HOLD_CAP,
                focus: Some((wid(1), rect(4.0, 32.0, 859.0, 1081.0))),
                background: vec![(wid(9), rect(224.0, 95.0, 600.0, 400.0))],
                background_initial: Some(now + Duration::from_millis(30)),
                behind: vec![wid(9)],
            }
        }

        #[test]
        fn the_end_state_is_watched_one_slot_a_kind_and_retargeted_from_one_generation_to_the_next() {
            let (pictures, mut commands) = Pictures::detached();
            let client = Client(1);
            let now = Instant::now();
            let mut watches = EndWatches::default();
            watches.watch(&pictures, client, &end(3, now));
            let tag = Tag { client, generation: 3 };
            let first = asked(&mut commands);
            assert_eq!(first.len(), 4, "{first:?}");
            match &first[0] {
                Asked::Watch(id, Watch::Landing { tag: under, windows, behind, slack, settle, until }) => {
                    assert_eq!(Some(*id), watches.landing);
                    assert_eq!(*under, tag);
                    let target = &windows[0].0;
                    assert_eq!((target.window, target.server_id), (wid(1), WindowServerId::new(1)));
                    assert_eq!(*behind, vec![WindowServerId::new(9)], "the strip is captured over them");
                    assert_eq!((*slack, *settle, *until), (END_SLACK, Settle::AsSoonAsThere, now + Duration::from_millis(150)));
                }
                other => panic!("{other:?}"),
            }
            match &first[1] {
                Asked::Watch(id, Watch::Landing { windows, settle, until, .. }) => {
                    assert_eq!(Some(*id), watches.awaiting);
                    assert_eq!(windows[0].0.window, wid(2));
                    assert_eq!((*settle, *until), (Settle::Repainted, now + HOLD_CAP), "watched as long as it is held for");
                }
                other => panic!("{other:?}"),
            }
            match &first[2] {
                Asked::Watch(id, Watch::Change { window, every, tries, .. }) => {
                    assert_eq!(Some(*id), watches.change);
                    assert_eq!(window.window, wid(1));
                    assert_eq!((*every, *tries), (CHANGE_EVERY, CHANGE_TRIES));
                }
                other => panic!("{other:?}"),
            }
            match &first[3] {
                Asked::Watch(_, Watch::Live { windows, every, after, initial, .. }) => {
                    assert_eq!(windows[0].window, wid(9));
                    assert_eq!(*every, LIVE_EVERY);
                    assert_eq!(*after, watches.change, "kept fresh once the focus target's look is in");
                    assert_eq!(*initial, Some(now + Duration::from_millis(30)));
                }
                other => panic!("{other:?}"),
            }

            // The next generation: everything has landed but what repaints, and the focus is in.
            let mut next = end(4, now);
            next.landing.clear();
            next.focus = None;
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
            assert_eq!((watches.awaiting, watches.live), (awaiting, live), "retargeted, not watched afresh");
            assert_eq!((watches.landing, watches.change), (None, None));

            watches.stop(&pictures);
            assert!(matches!(&asked(&mut commands)[..], [Asked::Stop(_), Asked::Stop(_)]));
            assert_eq!(watches, EndWatches::default());
        }

        #[test]
        fn an_ask_of_windows_on_screen_carries_the_engines_tag_and_each_window_where_it_is_expected() {
            let client = Client(1);
            let tag = Tag { client, generation: 5 };
            let at: ScreenRect = rect(4.0, 32.0, 859.0, 1081.0);
            let floating = rect(224.0, 95.0, 600.0, 400.0);
            let ask = on_screen(tag, Moment::FrameZero, &[(wid(1), at)], &[(wid(9), floating)], &[wid(9)]);
            assert_eq!((ask.tag, ask.moment), (tag, Moment::FrameZero));
            let size = at.size;
            let target = PictureTarget { window: wid(1), server_id: WindowServerId::new(1), size };
            assert_eq!(ask.strip, vec![(target, at)]);
            assert_eq!(ask.background.len(), 1);
            assert_eq!(ask.behind, vec![WindowServerId::new(9)]);
            assert_eq!(sized_targets(vec![(wid(1), at)]), vec![target], "warmed at the size it ended");
        }

        /// A turned look keeps the size it had, so a warm would drop it; asked at focus priority, it
        /// would start ScreenCaptureKit through the hold of a flight the same click begins.
        #[test]
        fn a_turned_look_is_asked_at_background_priority_at_the_size_it_is_at() {
            let at: ScreenRect = rect(4.0, 32.0, 859.0, 1081.0);
            let ask = refresh(vec![(wid(1), at)]);
            assert_eq!(ask.priority, Priority::Background);
            assert_eq!(ask.targets, sized_targets(vec![(wid(1), at)]));
        }

        #[test]
        fn the_coordinator_hears_every_tile_and_reserved_entrance_in_screen_points() {
            let display = rect(1728.0, 0.0, 1728.0, 1117.0);
            let slot = rect(4.0, 32.0, 859.0, 1081.0);
            let next = rect(867.0, 32.0, 859.0, 1081.0);
            let mut running = flight();
            running.tiles.push(stacked(wid(1), next, slot, Some(0), false));
            let mut border = stacked(wid(8), slot, slot, None, false);
            border.companion = Some(wid(1));
            running.tiles.push(border);
            running.entrances.push(PendingEntrance { window: wid(2), to: next, floating: true });
            running.final_frames = vec![(wid(1), from_overlay_space(slot, display))];
            running.focus = Some(wid(2));
            running.shown_focus = Some(wid(1));
            let stage = running.stage(layout(vec![wid(2)]), display, &|window| window == wid(1));
            assert_eq!(
                stage.tiles[0],
                TileSummary {
                    window: wid(1),
                    start: rect(2595.0, 32.0, 859.0, 1081.0),
                    end: rect(1732.0, 32.0, 859.0, 1081.0),
                    floating: false,
                    companion: false,
                }
            );
            assert!(stage.tiles[1].companion);
            assert_eq!(
                stage.tiles[2],
                TileSummary {
                    window: wid(2),
                    start: from_overlay_space(entrance_from(next), display),
                    end: rect(2595.0, 32.0, 859.0, 1081.0),
                    floating: true,
                    companion: false,
                },
                "a reserved entrance, drawn once its picture is swapped in"
            );
            assert_eq!(stage.final_frames, running.final_frames);
            assert_eq!(stage.losing_focus, Some(wid(1)), "drawn focused until focus hands over");
            assert_eq!(stage.opaque, vec![wid(1)]);
            assert_eq!((stage.awaiting, stage.display), (vec![wid(2)], display));
            running.shown_focus = Some(wid(2));
            assert_eq!(running.stage(layout(Vec::new()), display, &|_| false).losing_focus, None, "handed over");
        }
    }

    /// What the flight composed, as the coordinator's commands change it.
    mod composed {
        use super::*;
        use crate::pictures::domain::knowledge::CORNER_RADIUS;

        #[test]
        fn frame_zeros_pictures_are_worn_by_their_tiles_and_never_by_a_border() {
            let slot = rect(4.0, 32.0, 859.0, 1081.0);
            let mut running = flight();
            running.tiles.push(stacked(wid(1), slot, slot, Some(0), false));
            let mut border = stacked(wid(8), slot, slot, None, false);
            border.companion = Some(wid(1));
            running.tiles.push(border);
            let fresh = test_picture(ScreenSize::new(859.0, 1000.0));
            let worn = running.wear(vec![(wid(1), fresh.clone()), (wid(8), fresh.clone()), (wid(5), fresh.clone())]);
            assert_eq!(worn, 1, "a window with no tile is the cache's alone");
            assert_eq!(running.tiles[0].snapshot.coverage, fresh.coverage);
            assert_ne!(running.tiles[1].snapshot.coverage, fresh.coverage);
        }

        #[test]
        fn a_reserved_entrance_joins_with_its_first_picture_once_frontmost_and_focused() {
            let slot = rect(867.0, 32.0, 859.0, 1081.0);
            let neighbour = rect(4.0, 32.0, 859.0, 1081.0);
            let mut running = flight();
            running.tiles.push(stacked(wid(1), neighbour, neighbour, Some(1), false));
            running.entrances.push(PendingEntrance { window: wid(2), to: slot, floating: false });
            running.focus = Some(wid(2));
            let tile = running.admit(wid(2), &test_picture(slot.size)).expect("admitted");
            assert_eq!((tile.from, tile.to), (entrance_from(slot), slot), "zero width at its left edge");
            assert!(tile.focused);
            assert_eq!(tile.depth, 0, "the entrance leads: raised on open");
            assert!(running.entrances.is_empty());
            let other = running.tiles.iter().find(|t| t.window == wid(1)).unwrap();
            assert_eq!(other.depth, 2, "its neighbour keeps the server's order, within the band");
            assert!(running.admit(wid(2), &test_picture(slot.size)).is_none(), "taken once");
            assert!(running.admit(wid(1), &test_picture(slot.size)).is_none(), "not an entrance");
            assert_eq!(running.tiles.len(), 2, "no second tile");
        }

        /// A stand-in covers nothing, so its window is awaited like a grow; its real picture, once
        /// swapped in, goes onto the same tile, which moves on as it was.
        #[test]
        fn a_stand_in_takes_its_real_picture_on_the_same_tile() {
            use crate::pictures::platform::picture::{stand_in_image, stand_in_picture};
            let park = rect(1727.0, 32.0, 859.0, 1081.0);
            let slot = rect(4.0, 32.0, 859.0, 1081.0);
            let (image, _) = stand_in_image(2.0).expect("drawn");
            let stand_in = stand_in_picture(slot.size.respace(), image);
            assert_eq!(EndPicture::of_picture(stand_in.coverage.covered, slot.size.respace()), EndPicture::Awaited);
            let mut running = flight();
            let mut tile = stacked(wid(1), park, slot, Some(0), false);
            tile.snapshot = stand_in;
            running.tiles.push(tile);
            let real = test_picture(slot.size);
            assert!(matches!(running.take_picture(wid(1), &real), Took::Tile { size } if size == slot.size.respace()));
            assert_eq!(running.tiles.len(), 1, "one tile, never a newcomer's entrance");
            let tile = &running.tiles[0];
            assert_eq!((tile.from, tile.to), (park, slot), "travels as it did");
            assert_ne!(tile.snapshot.source, Route::StandIn);
            assert!(running.entrances.is_empty());
        }

        /// A reserved entrance joins with its picture; a window with a tile takes it in place, and
        /// stops flying a focused look; a window nothing draws takes nothing.
        #[test]
        fn a_swapped_in_picture_admits_an_entrance_or_goes_onto_its_windows_tile() {
            let slot = rect(867.0, 32.0, 859.0, 1081.0);
            let mut running = flight();
            running.tiles.push(stacked(wid(1), slot, slot, Some(0), false));
            running.entrances.push(PendingEntrance { window: wid(2), to: slot, floating: false });
            running.focused_look = Some(wid(1));
            let picture = test_picture(slot.size);
            assert!(matches!(running.take_picture(wid(2), &picture), Took::Entrance(tile) if tile.from == entrance_from(slot)));
            assert!(matches!(running.take_picture(wid(1), &picture), Took::Tile { .. }));
            assert_eq!(running.focused_look, None);
            assert!(matches!(running.take_picture(wid(3), &picture), Took::Nothing));
            assert_eq!(running.tiles.len(), 2);
        }

        /// Two columns stepping one column left, frame zero posed.
        fn posed() -> (RunningAnimation, plan::ReflowPlan) {
            let (a, b) = (rect(4.0, 32.0, 859.0, 1081.0), rect(867.0, 32.0, 859.0, 1081.0));
            let shifted = |frame: ScreenRect, dx: f64| rect(frame.origin.x + dx, frame.origin.y, frame.size.width, frame.size.height);
            let requests = [(wid(1), a, shifted(a, -863.0), false), (wid(2), b, shifted(b, -863.0), false)];
            let mut running = flight();
            running.tiles = requests.iter().map(|&(w, from, to, _)| stacked(w, on(from), on(to), Some(0), false)).collect();
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
            Stage { plan, drawn, seconds: 0.35, cue: Cue::Plain, accents: &[] }
        }

        fn strip_key(running: &RunningAnimation) -> plan::GroupKey {
            running.plan.groups.iter().find(|g| g.members.iter().any(|m| m.window == wid(1))).map(|g| g.key).expect("rigid")
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
            assert!(start.contains(&Edit::Add(Bounce::from_rest(right, plays).track(key, 10.0))), "the latest push");
            assert!(!start.contains(&Edit::Add(Bounce::from_rest(left, plays).track(key, 10.0))));
            let pushed = running.push(&Cue::Plain, OverlayVector::new(0.0, 72.0), 0.35, 10.1).expect("at once");
            assert!(!pushed.is_empty());
            assert!(pushed.iter().all(|e| matches!(e, Edit::Add(t) if t.begin() == 10.1)), "{pushed:?}");
        }

        /// A pass before the flight moves is folded into frame zero with its step waiting for the
        /// motion, which takes it as it starts; one once the flight moves bends it, the tiles following
        /// the merged plan.
        #[test]
        fn a_pass_before_the_flight_moves_is_folded_with_its_step_waiting_and_one_after_bends_it() {
            use crate::animation::domain::motion::score::Channel;
            let (mut running, again) = posed();
            let step = Accent::Step { offset: OverlayVector::new(-576.0, 0.0), seconds: 0.35 };
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
            assert_eq!(running.plan.position_of(key), OverlayPoint::new(-1726.0, 0.0), "both pans from frame zero");
            pose(&mut running);
            running.flown = true;
            let (plan, drawn) = (running.plan.clone(), running.drawn());
            let start = running.score.start_moving(&stage(&plan, &drawn), 10.0);
            assert!(start.iter().any(|e| matches!(e, Edit::Add(t) if t.channel() == Channel::Nudge && t.begin() == 10.0)), "the step waited");

            let further = Pass { pan: Some(OverlayVector::new(-863.0, 0.0)), accent: None, ..pass };
            let (delta, edits) = running.take_pass(&further, 10.1).expect("bent");
            assert!(delta.retargeted_groups.iter().any(|(k, _)| *k == key));
            assert!(edits.iter().any(|e| matches!(e, Edit::Add(t) if t.begin() == 10.1)));
            assert_eq!(running.plan.position_of(key), OverlayPoint::new(-2589.0, 0.0));
            let tile = running.tiles.iter().find(|t| t.window == wid(1)).unwrap();
            assert_eq!(tile.to.origin.x, 4.0 - 2589.0, "the tile follows the merged plan");
        }

        #[test]
        fn focus_handing_over_draws_the_target_focused_and_the_window_left_not() {
            let slot = rect(4.0, 32.0, 859.0, 1081.0);
            let mut running = flight();
            running.tiles = vec![
                stacked(wid(1), slot, slot, Some(0), false),
                stacked(wid(2), slot, slot, Some(1), false),
                stacked(wid(3), slot, slot, Some(2), false),
            ];
            running.tiles[0].focused = true;
            running.focus = Some(wid(2));
            running.shown_focus = Some(wid(1));
            running.hand_over_focus(wid(2), Some(wid(1)));
            let focused: Vec<bool> = running.tiles.iter().map(|t| t.focused).collect();
            assert_eq!(focused, vec![false, true, false]);
            assert_eq!(losing_focus(running.focus, running.shown_focus), None, "a later merge moves none");
        }

        #[test]
        fn a_skylight_picture_baked_with_the_desktop_is_clipped_to_its_corners_and_no_other_is() {
            let size = TileSize::new(859.0, 1081.0);
            let mut skylight = test_picture(size);
            skylight.source = Route::SkyLight;
            let baked = Knowledge { translucent: Some(false), material: Some(true), radius: Some(12.0) };
            assert_eq!(corner_clip(baked, &skylight, size), Some(12.0));
            let unmeasured = Knowledge { translucent: Some(false), material: None, radius: None };
            assert_eq!(corner_clip(unmeasured, &skylight, size), Some(CORNER_RADIUS), "baked, its radius not measured");
            let translucent = Knowledge { translucent: Some(true), ..baked };
            assert_eq!(corner_clip(translucent, &skylight, size), None, "captured alone, its corners clear");
            let plain = Knowledge { material: Some(false), ..baked };
            assert_eq!(corner_clip(plain, &skylight, size), None);
            assert_eq!(corner_clip(baked, &test_picture(size), size), None, "ScreenCaptureKit leaves the corners clear");
        }

        #[test]
        fn a_parked_focus_target_flies_its_focused_look_only_where_it_fits() {
            let park = rect(-860.0, 32.0, 859.0, 1081.0);
            let mut running = flight();
            running.tiles.push(stacked(wid(2), park, rect(4.0, 32.0, 859.0, 1081.0), Some(0), false));
            assert!(!running.fly_focused_look(wid(2), Some(test_picture(ScreenSize::new(600.0, 1081.0)))), "drawn squashed");
            assert!(!running.fly_focused_look(wid(2), None));
            assert!(!running.fly_focused_look(wid(3), Some(test_picture(park.size))), "no tile");
            assert_eq!((running.focused_look, running.tiles[0].focused), (None, false));
            let mut look = test_picture(park.size);
            look.coverage.covered.height -= 1.0;
            assert!(running.fly_focused_look(wid(2), Some(look.clone())));
            assert_eq!(running.tiles[0].snapshot.coverage, look.coverage);
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
            assert!(running.fly_focused_look(wid(2), Some(look.clone())));
            let next = rect(867.0, 32.0, 859.0, 1081.0);
            let outcomes = running.merge_pass(vec![stacked(wid(2), park, next, Some(0), false)], None);
            assert_eq!(outcomes, vec![(wid(2), Admitted::Retargeted)]);
            let tile = running.tiles.iter().find(|t| t.window == wid(2)).unwrap();
            assert_eq!(tile.to, next);
            assert_eq!(tile.snapshot.coverage, look.coverage, "not the cache's");
            assert!(tile.focused);
            assert_eq!(running.focused_look, Some(wid(2)));
            running.merge_pass(vec![stacked(wid(2), park, slot, Some(0), false)], Some(wid(2)));
            assert_eq!(running.focused_look, Some(wid(2)), "a pass naming the same focus moves none");
        }

        /// A pass moving focus on: the window focused as the flight began is still drawn focused
        /// until focus hands over, the target before it no longer is, and nothing flies a focused look
        /// until the new target is given its own.
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
            assert!(running.fly_focused_look(wid(2), Some(test_picture(park.size))));
            running.merge_pass(vec![stacked(wid(3), park, slot, Some(2), false)], Some(wid(3)));
            let focused: Vec<(WindowId, bool)> = running.tiles.iter().map(|t| (t.window, t.focused)).collect();
            assert_eq!(focused, vec![(wid(1), true), (wid(2), false), (wid(3), false)]);
            assert_eq!(running.focused_look, None);
        }

        #[test]
        fn a_strip_window_on_this_display_with_no_usable_picture_is_placed_but_not_drawn() {
            use crate::animation::domain::motion::surface::surface_travel;
            use crate::pictures::domain::coverage::needs_capture;
            let mut rng = Gen(95);
            for _ in 0..RUNS {
                let slot: SurfaceRect = rng.on_screen();
                let mut cache: crate::pictures::domain::cache::PictureCache<Picture> =
                    crate::pictures::domain::cache::PictureCache::new();
                cache.insert(wid(2), clipped(slot.size.respace()));
                assert!(cache.usable(wid(1)).is_none(), "seed 95: never captured");
                assert!(cache.usable(wid(2)).is_none(), "seed 95: a sliver");
                assert!(cache.usable(wid(3)).is_none());
                cache.insert(wid(3), test_picture(slot.size));
                assert!(cache.usable(wid(3)).is_some(), "seed 95: the drawn neighbour");

                let (from, to) =
                    surface_travel(slot, SurfacePoint::origin(), SurfacePoint::new(861.0, 0.0), false);
                let mut running = flight();
                running.tiles.push(stacked(wid(3), from, to, Some(0), false));
                let real_to = screen(to);
                running.final_frames = vec![(wid(1), real_to), (wid(2), real_to), (wid(3), real_to)];
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

        fn composed((tiles, pieces): (Vec<OverlayTile>, plan::ReflowPlan), focus: WindowId) -> RunningAnimation {
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

        fn piece(running: &RunningAnimation, window: WindowId) -> Option<(plan::GroupKey, OverlayVector)> {
            let group = running.plan.groups.iter().find(|g| g.members.iter().any(|m| m.window == window))?;
            Some((group.key, group.travel))
        }

        /// Kai, 2026-10-03: "rapid press navigation messes things up, i periodically see missing
        /// tiles". A switch pressed again before the first moves travels the whole stack as one
        /// surface: the floating window rides its container both rows, and the row the second press
        /// brings starts below the one the first brought, where the stack has it, not on top of it.
        #[test]
        fn a_second_switch_before_the_flight_moves_carries_the_floating_window_with_the_surface() {
            let (a, b, floater, c) = (wid(1), wid(2), wid(3), wid(4));
            let column = |row: f64| -> SurfaceRect { rect(4.0, 32.0 + row * PITCH, 1720.0, 1081.0) };
            let floating = |row: f64| -> SurfaceRect { rect(224.0, 95.0 + row * PITCH, 600.0, 400.0) };
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
            assert_eq!(running.plan.floating_travel, both, "the floating container travels both rows");
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
            assert_eq!(path(&running, c), (seen(column(2.0)), seen(column(0.0))), "below B, where the stack has it");
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
            let at = |i: usize, scrolled: f64| on_surface(columns[i], column(4.0 + (i as f64 - scrolled) * step), false);
            let first: Vec<SurfaceWindow> = (0..3).map(|i| at(i, 1.0)).collect();
            let second: Vec<SurfaceWindow> = (1..4).map(|i| at(i, 2.0)).collect();
            let (away, rest) = (SurfacePoint::new(-step, 0.0), SurfacePoint::new(0.0, 0.0));
            let mut running = composed(movement(&first, away, rest), columns[2]);
            press(&mut running, &second, (away, rest), columns[3], Cue::Plain);

            let strip = piece(&running, columns[0]);
            assert_eq!(strip.map(|(_, travel)| travel), Some(OverlayVector::new(-2.0 * step, 0.0)));
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
            let first = [on_surface(a, column(4.0), false), on_surface(w, column(4.0 + step), false)];
            let second = [on_surface(w, column(4.0), false), on_surface(a, column(4.0 + step), false)];
            let rest = SurfacePoint::new(0.0, 0.0);
            let mut running = composed(movement(&first, rest, rest), a);
            assert_eq!(running.cue, Cue::Plain);
            let moved = Cue::Move { window: w, direction: rini_ipc::protocol::Direction::Left };
            press(&mut running, &second, (rest, rest), w, moved);
            assert_eq!(running.cue, moved);
            press(&mut running, &second, (rest, rest), w, Cue::Plain);
            assert_eq!(running.cue, Cue::Plain, "the latest, plain or not");
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

        fn column(i: f64) -> OverlayRect {
            rect(4.0 + i * 863.0, 32.0, 859.0, 1081.0)
        }

        /// The plan of `tiles` alone, each adopted as a pass adopts a tile it did not derive.
        fn plan_of(tiles: &[OverlayTile]) -> FlightPlan {
            let mut pieces = ReflowPlan::empty();
            for tile in tiles {
                pieces.adopt(tile);
            }
            FlightPlan::from(pieces)
        }

        fn shifted(frame: OverlayRect, dx: f64, dy: f64) -> OverlayRect {
            OverlayRect::new(
                Point::new(frame.origin.x + dx, frame.origin.y + dy),
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
            use crate::animation::domain::motion::z_group::GROUP_STRIDE;
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
            let mut companion = stacked(wid(900), left, left, None, false);
            companion.companion = Some(wid(90));
            tiles.push(companion);
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
                let (from_offset, to_offset) = (SurfacePoint::new(-step, 0.0), SurfacePoint::new(0.0, 0.0));
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
                .map(|(order, &(window, frame))| stacked(window, seen(frame), seen(frame), Some(order), false))
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

    /// Border windows ride the window they trace: its container, its band, its step.
    mod borders {
        use rini_ipc::protocol::Direction;

        use super::*;
        use super::wid;
        use crate::animation::domain::motion::plan::*;
        use crate::animation::domain::motion::surface::SurfaceGeometry;
        use crate::transitions::domain::strip_move::tests::staging::*;

        /// A border four points proud of `frame` all round.
        fn hug<U>(frame: Rect<U>) -> Rect<U> {
            rect(
                frame.origin.x - 4.0,
                frame.origin.y - 4.0,
                frame.size.width + 8.0,
                frame.size.height + 8.0,
            )
        }

        /// A window on the strip surface where the window server has it, the viewport at rest.
        fn real(frame: SurfaceRect) -> ScreenRect { screen(seen(frame)) }

        fn floating_at(window: WindowId, frame: SurfaceRect, pinned: bool) -> SurfaceWindow {
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
            let settings: SurfaceRect = rect(500.0, 300.0, 700.0, 500.0);
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
            assert_eq!(step, OverlayVector::new(-576.0, 0.0), "away from where W went");
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
            let border = |anchor: WindowId, at: SurfaceRect, window| {
                let real = real(at);
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
                    from: installed(seen(hug(settings))),
                    to: installed(seen(hug(settings)))
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
            let settings: SurfaceRect = rect(500.0, 300.0, 700.0, 500.0);
            let windows = [floating_at(f, settings, false)];
            let from_offset = SurfacePoint::new(0.0, -1117.0);
            let mut plan = surface_plan(&windows, from_offset, REST);
            assert!(plan.floating_travel.y != 0.0, "the container travels");
            let (from, to) = windows[0].travel(from_offset, REST);
            let border = border_tile(
                &stacked(f, from, to, Some(0), true),
                real(settings),
                (wid(70), hug(real(settings))),
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
            let real = real(at(4.0, WIDTH));
            let frame = rect(0.0, 28.0, WIDTH + 8.0, 1089.0);
            let border = border_tile(&anchor, real, (wid(20), frame), test_picture(frame.size));
            incoming.adopt(&border);

            let along = halfway(&current);
            let key = key_of(&current, w).unwrap();
            let on_screen: HashMap<GroupKey, OverlayPoint> =
                along.iter().map(|(key, p)| (*key, OverlayPoint::new(p.x - 576.0, p.y))).collect();
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
                VIEWPORT,
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
}
