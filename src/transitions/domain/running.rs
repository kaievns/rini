//! A flight as composed (`RunningAnimation`): its tiles and the pictures they draw, its plan and
//! score, the entrances waiting for a picture and the passes held; and the rules over it, each a pure
//! change of what it holds: a later pass merged into its tiles and score, a reserved entrance joining
//! with its first picture, frame zero's pictures and each one swapped in, a repaint held for its
//! tile's corner, the focus handover and a parked target's focused look, the tiles the coordinator is
//! told of, and how the tiles band in depth. Generic over the picture a tile is drawn with
//! (`TilePicture`), so it is tested over plain sizes and routes; the engine
//! (`crate::transitions::platform::engine`) carries it out on the renderer.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use rini_core::ids::WindowId;
use rini_geometry::from_overlay_space;
use rini_geometry::space::{
    OverlayRect, OverlayVector, PointSpace, Respace, ScreenRect, ScreenSize, Size,
};
use rini_geometry::stretch::grows;

use crate::animation::domain::choreography::{Cue, Look, Pass, Stage};
use crate::animation::domain::motion::accents::Accent;
use crate::animation::domain::motion::plan::{self, entrance_from};
use crate::animation::domain::motion::score::{Edit, Score};
use crate::animation::domain::motion::tile::{self, OverlayTile};
use crate::animation::domain::motion::z_group::{Band, Stacked, focus_is_off_strip, stack};
use crate::decorations::domain::shape::outset;
use crate::decorations::domain::style::Worn;
use crate::pictures::domain::cache::HasCoverage;
use crate::pictures::domain::route::Route;
use crate::transitions::domain::admission::{
    Admitted, PendingEntrance, Waits, merge_action, retarget_entrances,
};
use crate::transitions::domain::flight_score::FlightScore;
use crate::transitions::domain::lifecycle::{
    Kind, StageSummary, Start, TileSummary, drawn_focused, extend_latest, losing_focus,
};
use crate::windows::domain::raise_order::StackGroup;

/// What the flight's rules read of the picture a tile is drawn with: how much of its window it
/// covers, which way it was taken, and whether it carries its window's blur. The pixels are the
/// platform's.
pub(in crate::transitions) trait TilePicture: Clone + HasCoverage {
    fn source(&self) -> Route;
    fn carries_blur(&self) -> bool;
    fn taken(&self) -> Instant;
    /// Whether it draws at `size` without visibly distorting it.
    fn fits<U: PointSpace>(&self, size: Size<U>) -> bool;
}

/// What a flight composed: its tiles and the pictures they draw, its plan, the entrances still
/// waiting for a picture, and what the renderer has been handed. When any of it changes is the
/// coordinator's.
pub(in crate::transitions) struct RunningAnimation<P> {
    pub(in crate::transitions) tiles: Vec<OverlayTile<P>>,
    /// Where each window must end up, across every pass merged in.
    pub(in crate::transitions) final_frames: Vec<(WindowId, ScreenRect)>,
    /// Windows drawn once a first picture of them is swapped in.
    pub(in crate::transitions) entrances: Vec<PendingEntrance>,
    /// The window gaining focus, from the latest pass that named one; its group is banded in front.
    pub(in crate::transitions) focus: Option<WindowId>,
    /// The window focus is drawn leaving until it hands over: the one focused as the flight began,
    /// then the target drawn focused.
    pub(in crate::transitions) shown_focus: Option<WindowId>,
    /// The key window as the flight's first pass went out.
    pub(in crate::transitions) key: Option<WindowId>,
    /// The focus target whose tile flies its focused look, until a picture is swapped onto it.
    pub(in crate::transitions) focused_look: Option<WindowId>,
    /// The flight as rigid pieces, which its look composes the score from.
    /// See "The overlay engine" in `src/animation/docs/animation-smoothness.md`.
    pub(in crate::transitions) plan: plan::FlightPlan,
    /// The plan is playing on the renderer: a pass or a picture now retargets it rather than
    /// installing frame zero again.
    pub(in crate::transitions) flown: bool,
    /// Everything the flight draws, and the accents waiting for its motion.
    pub(in crate::transitions) score: FlightScore,
    /// The press frame zero shows: the latest folded into it.
    pub(in crate::transitions) cue: Cue,
    /// Passes that reached the flight while it moved, oldest first, not merged until the coordinator
    /// says (`Command::Retarget`).
    pub(in crate::transitions) held: Vec<Composed<P>>,
}

/// What a swapped-in picture became.
pub(in crate::transitions) enum Took<P> {
    /// A reserved entrance's tile, joining the flight.
    Entrance(Box<OverlayTile<P>>),
    /// The picture on the window's tile.
    Tile,
    /// The picture its tile's leg in two steps flies from the corner on.
    Corner,
    /// Nothing draws the window.
    Nothing,
}

/// Writes every tile's destination back from the merged plan: a member the pass did not compose
/// rides its group all the same.
fn sync_tiles_to_plan<P>(tiles: &mut [OverlayTile<P>], plan: &plan::FlightPlan) {
    for tile in tiles.iter_mut() {
        if let Some(to) = plan.destination(tile.window) {
            tile.to = to;
        }
    }
}

/// The z rule's view of every tile, in tile order.
fn stacked_windows<P>(tiles: &[OverlayTile<P>]) -> Vec<Stacked> {
    tiles
        .iter()
        .map(|t| Stacked {
            window: t.window,
            group: StackGroup::of(t.floating),
            server_order: t.server_order,
        })
        .collect()
}

/// Depth for every tile, banded by `z_group::stack`.
/// The reactor's regroup matches it. See "The overlay engine" in `src/animation/docs/animation-smoothness.md`.
pub(in crate::transitions) fn restack<P>(tiles: &mut [OverlayTile<P>], focus: Option<WindowId>) {
    let placements = stack(&stacked_windows(tiles), focus);
    for (tile, placement) in tiles.iter_mut().zip(placements) {
        tile.depth = placement.depth;
    }
}

/// The flight's z-order as containers: `container_z - within` reproduces `-depth`.
/// See "The overlay engine" in `src/animation/docs/animation-smoothness.md`.
pub(in crate::transitions) fn band_plan<P>(
    plan: &plan::FlightPlan,
    tiles: &[OverlayTile<P>],
    focus: Option<WindowId>,
) -> plan::Banding {
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
pub(in crate::transitions) struct Shape {
    pub(in crate::transitions) kind: Kind,
    pub(in crate::transitions) start: Start,
    pub(in crate::transitions) duration: Duration,
    pub(in crate::transitions) waits: Waits,
}

/// One pass, composed: what `begin_group` poses or merges.
pub(in crate::transitions) struct Composed<P> {
    pub(in crate::transitions) tiles: Vec<OverlayTile<P>>,
    pub(in crate::transitions) final_frames: Vec<(WindowId, ScreenRect)>,
    pub(in crate::transitions) label: &'static str,
    pub(in crate::transitions) shape: Shape,
    pub(in crate::transitions) entrances: Vec<PendingEntrance>,
    /// The focus it names, and the window focused as it began.
    pub(in crate::transitions) focus: (Option<WindowId>, Option<WindowId>),
    /// The key window as it went out.
    pub(in crate::transitions) key: Option<WindowId>,
    pub(in crate::transitions) pan: Option<OverlayVector>,
    pub(in crate::transitions) plan: plan::ReflowPlan,
    /// How a flight it begins moves; a pass merged into one is moved by that flight's.
    pub(in crate::transitions) look: Look,
    pub(in crate::transitions) cue: Cue,
    pub(in crate::transitions) accent: Option<Accent>,
}

/// What the coordinator is told of a stage.
#[allow(clippy::too_many_arguments, reason = "no roadmap task groups them")]
pub(in crate::transitions) fn summary(
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

impl<P: TilePicture> RunningAnimation<P> {
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
    pub(in crate::transitions) fn stage(
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
    pub(in crate::transitions) fn wear(
        &mut self,
        pictures: Vec<(WindowId, P, bool)>,
    ) -> Vec<WindowId> {
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
    pub(in crate::transitions) fn worn(
        &self,
        windows: &[WindowId],
    ) -> Vec<(WindowId, Route, bool, Instant)> {
        self.tiles
            .iter()
            .filter(|tile| windows.contains(&tile.window))
            .map(|tile| {
                (
                    tile.window,
                    tile.snapshot.source(),
                    tile.snapshot.carries_blur(),
                    tile.snapshot.taken(),
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
        picture: &P,
        clipped: bool,
        radius: impl FnOnce() -> Option<f64>,
    ) -> Option<OverlayTile<P>> {
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
    pub(in crate::transitions) fn take_picture(
        &mut self,
        window: WindowId,
        picture: &P,
        clipped: bool,
        radius: impl FnOnce() -> Option<f64>,
        at: f64,
    ) -> Took<P> {
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
    fn waits_for_corner(&self, window: WindowId, picture: &P, at: f64) -> bool {
        let Some(tile) = self.tiles.iter().find(|t| t.window == window) else {
            return false;
        };
        let start = self
            .score
            .leg_start(window)
            .filter(|_| self.flown)
            .map_or(tile.from.size, Respace::respace);
        let (covered, source) = (tile.snapshot.coverage().covered, tile.snapshot.source());
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
    fn press_covers<U: PointSpace>(&self, tile: &OverlayTile<P>, to: Size<U>, at: f64) -> bool {
        let press = tile.snapshot.coverage().covered;
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
    pub(in crate::transitions) fn carried_into(
        &self,
        window: WindowId,
        to: ScreenSize,
        at: f64,
    ) -> Option<ScreenSize> {
        let tile = self.tiles.iter().find(|tile| tile.window == window)?;
        let ahead = self.score.corner_at(window).is_some_and(|corner| at < corner);
        let held = tile.from_corner.as_ref().filter(|_| !ahead || !self.press_covers(tile, to, at));
        Some(held.unwrap_or(&tile.snapshot).coverage().covered)
    }

    /// `target`'s tile flies `look`, drawn focused and `clipped` or whole, if it fits where the tile
    /// starts. Whether it does.
    pub(in crate::transitions) fn fly_focused_look(
        &mut self,
        target: WindowId,
        look: Option<P>,
        clipped: bool,
    ) -> bool {
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
    pub(in crate::transitions) fn draw_focused(&mut self, window: WindowId, focused: bool) {
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
    pub(in crate::transitions) fn merge_pass(
        &mut self,
        tiles: Vec<OverlayTile<P>>,
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
    pub(in crate::transitions) fn take_pass(
        &mut self,
        pass: &Pass,
        at: f64,
    ) -> Option<(plan::PlanDelta, Vec<Edit>)> {
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

    /// Frame zero posed again, its plan as it is, with the pictures the tiles hold now and banded as
    /// they are; the accents waiting for the motion keep waiting.
    pub(in crate::transitions) fn pose(&mut self) -> Score {
        let banding = band_plan(&self.plan, &self.tiles, self.focus);
        let drawn = self.drawn();
        let stage = Stage {
            plan: &self.plan,
            drawn: &drawn,
            seconds: 0.0,
            cue: self.cue,
            accents: &[],
        };
        self.score.pose(&stage, banding).clone()
    }

    /// The motion starting at `at` on the media clock, for `seconds`: the look composes it from frame
    /// zero with the accents that waited, and each picture held for a corner no longer ahead is its
    /// tile's own. The edits that play it.
    pub(in crate::transitions) fn start_moving(&mut self, seconds: f64, at: f64) -> Vec<Edit> {
        self.flown = true;
        let drawn = self.drawn();
        let stage = Stage {
            plan: &self.plan,
            drawn: &drawn,
            seconds,
            cue: self.cue,
            accents: &[],
        };
        let edits = self.score.start_moving(&stage, at);
        self.turn_corners(at);
        edits
    }

    /// The tiles banded in depth as the plan and the focus have them now, as the score's edit.
    pub(in crate::transitions) fn bank(&mut self) -> Edit {
        let banding = band_plan(&self.plan, &self.tiles, self.focus);
        self.score.bank(banding)
    }

    /// A reserved entrance's tile, once its first picture made it one of the flight's, joining the
    /// motion: before it moves, laid in frame zero's loose container, and `None`, frame zero to be
    /// posed again; once it moves, merged in as a pass holding it alone for the `seconds` left of the
    /// flight, bent from the score at `at` and the tiles banded again, the edits that carry it out.
    pub(in crate::transitions) fn join(
        &mut self,
        tile: &OverlayTile<P>,
        viewport: OverlayRect,
        seconds: f64,
        at: f64,
    ) -> Option<Vec<Edit>> {
        if !self.flown {
            let loose = self.plan.position_of(plan::GroupKey::Loose);
            let (from, to) = (
                plan::group_relative(tile.from, loose),
                plan::group_relative(tile.to, loose),
            );
            self.plan.entrances.push((tile.window, from, to));
            return None;
        }
        let mut entrance = plan::ReflowPlan::empty();
        entrance.entrances.push((
            tile.window,
            plan::installed(tile.from),
            plan::installed(tile.to),
        ));
        let drawn = self.drawn();
        let pass = Pass {
            plan: &entrance,
            pan: None,
            focus: self.focus,
            viewport,
            drawn: &drawn,
            seconds,
            cue: Cue::Plain,
            accent: None,
        };
        let merged = self.score.merge(&self.plan, &pass, at);
        self.plan = merged.plan;
        let mut edits = merged.edits;
        edits.push(self.bank());
        Some(edits)
    }

    /// A push against `wall` reaching the flight: waiting for the motion until the flight moves, the
    /// latest push winning (`None`), and played on the score at `at` once it does.
    pub(in crate::transitions) fn push(
        &mut self,
        cue: &Cue,
        wall: OverlayVector,
        seconds: f64,
        at: f64,
    ) -> Option<Vec<Edit>> {
        if !self.flown {
            self.score.wait(Accent::Push { wall, seconds });
            return None;
        }
        Some(self.score.push(cue, wall, seconds, at))
    }

    /// A later pass's reserved entrances, but for a window already waiting to enter, and every
    /// entrance then pointed at where `final_frames` ends it on `display`, with those frames the
    /// flight's latest for their windows.
    pub(in crate::transitions) fn take_ends(
        &mut self,
        entrances: Vec<PendingEntrance>,
        final_frames: Vec<(WindowId, ScreenRect)>,
        display: Option<ScreenRect>,
    ) {
        for entrance in entrances {
            if !self.entrances.iter().any(|e| e.window == entrance.window) {
                self.entrances.push(entrance);
            }
        }
        if let Some(display) = display {
            retarget_entrances(&mut self.entrances, &final_frames, display);
        }
        extend_latest(&mut self.final_frames, final_frames.into_iter());
    }

    /// The windows the flight draws a tile of.
    pub(in crate::transitions) fn drawn(&self) -> Vec<WindowId> {
        self.tiles.iter().map(|tile| tile.window).collect()
    }

    /// How far the bands `window` wears at rest reach outside it on a display of `scale`, 0 with
    /// none: its border's, and its ring's if it wears the ring where the flight ends.
    pub(in crate::transitions) fn band(&self, window: WindowId, scale: f64) -> f64 {
        let worn = self.tiles.iter().find(|tile| tile.window == window).map(|tile| tile.worn);
        worn.map_or(0.0, |worn| {
            outset(&worn.decoration, worn.wears_ring.worn(), scale)
        })
    }

    /// Each end the overlay is holed over, with the outset of the band its window wears at rest.
    pub(in crate::transitions) fn holes(
        &self,
        ends: Vec<(WindowId, OverlayRect)>,
        scale: f64,
    ) -> Vec<(OverlayRect, f64)> {
        ends.into_iter().map(|(window, end)| (end, self.band(window, scale))).collect()
    }

    /// Adds or retargets one window without disturbing anything already moving, each tile already
    /// flying wearing what the pass ends with.
    fn merge(&mut self, tile: OverlayTile<P>) -> Admitted {
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

/// The tile for a reserved entrance whose picture has landed, `clipped` or whole and rounded at
/// `radius`: frontmost and focused, since a window is raised on open and about to hold focus.
fn entrance_tile<P: Clone>(
    entrance: &PendingEntrance,
    snapshot: &P,
    clipped: bool,
    radius: Option<f64>,
) -> OverlayTile<P> {
    let tile = TileOf {
        window: entrance.window,
        floating: entrance.floating,
        server_order: Some(0),
        worn: entrance.worn,
        radius,
    };
    let drawn = Drawn {
        snapshot: snapshot.clone(),
        clipped,
    };
    tile.flying((entrance_from(entrance.to), entrance.to), drawn, true)
}

/// Whose a tile is, and how it stacks and what it wears: what a tile is made of beside where it
/// flies and the picture it is drawn with.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(in crate::transitions) struct TileOf {
    pub(in crate::transitions) window: WindowId,
    pub(in crate::transitions) floating: bool,
    pub(in crate::transitions) server_order: Option<usize>,
    pub(in crate::transitions) worn: Worn,
    pub(in crate::transitions) radius: Option<f64>,
}

/// The picture a tile is drawn with, clipped to the tile's corner or whole.
#[derive(Clone, Debug)]
pub(in crate::transitions) struct Drawn<P> {
    pub(in crate::transitions) snapshot: P,
    pub(in crate::transitions) clipped: bool,
}

impl TileOf {
    /// Its tile flying `from` to `to` `drawn` so and drawn `focused` or not, at no depth until
    /// `restack` bands it.
    pub(in crate::transitions) fn flying<P>(
        self,
        (from, to): (OverlayRect, OverlayRect),
        Drawn { snapshot, clipped }: Drawn<P>,
        focused: bool,
    ) -> OverlayTile<P> {
        OverlayTile {
            window: self.window,
            from,
            to,
            snapshot,
            from_corner: None,
            floating: self.floating,
            server_order: self.server_order,
            depth: 0,
            focused,
            worn: self.worn,
            radius: self.radius,
            clipped,
        }
    }
}

#[cfg(test)]
mod tests {
    use rini_core::ids::WindowServerId;
    use rini_geometry::space::{OverlayPoint, Point, SurfacePoint, SurfaceRect};
    use rini_geometry::{SameAs, to_overlay_space};

    use super::*;
    use crate::animation::domain::motion::surface::{SurfaceGeometry, pan_travel, viewport};
    use crate::animation::domain::motion::testing::{DISPLAY, Gen, RUNS, VIEWPORT, rect};
    use crate::decorations::domain::style::Ring;
    use crate::pictures::domain::cache::CarriesOver;
    use crate::pictures::domain::coverage::{Coverage, fits_frame};
    use crate::transitions::domain::admission::{EndPicture, FliesOn};
    use crate::transitions::domain::flight::handover_report;
    use crate::transitions::domain::request::{AnimationRequest, SurfaceWindow};

    /// A picture as the flight's rules read it, with no pixels: `Picture`'s fields but its image.
    #[derive(Clone, Debug)]
    struct Shot {
        coverage: Coverage,
        source: Route,
        carries_blur: bool,
        taken: Instant,
    }

    impl HasCoverage for Shot {
        fn coverage(&self) -> Coverage { self.coverage }
    }

    impl CarriesOver for Shot {}

    impl TilePicture for Shot {
        fn source(&self) -> Route { self.source }

        fn carries_blur(&self) -> bool { self.carries_blur }

        fn taken(&self) -> Instant { self.taken }

        fn fits<U: PointSpace>(&self, size: Size<U>) -> bool {
            fits_frame(self.coverage.covered, size)
        }
    }

    type RunningAnimation = super::RunningAnimation<Shot>;
    type OverlayTile = tile::OverlayTile<Shot>;

    /// A picture covering the whole of a window of `size`, as a capture of its own surface is.
    fn test_picture<U: PointSpace>(size: Size<U>) -> Shot {
        Shot {
            coverage: Coverage {
                covered: size.respace(),
                window: size.respace(),
            },
            source: Route::ScreenCaptureKit,
            carries_blur: false,
            taken: Instant::now(),
        }
    }

    /// The stand-in for a window of `window` size with no picture, covering none of it.
    fn stand_in(window: ScreenSize) -> Shot {
        Shot {
            coverage: Coverage {
                covered: ScreenSize::zero(),
                window,
            },
            source: Route::StandIn,
            carries_blur: false,
            taken: Instant::now(),
        }
    }

    /// A SkyLight sliver: 40pt of an `size`-wide window, which `is_usable` rejects.
    fn clipped(size: ScreenSize) -> Shot {
        let mut snapshot = test_picture(size);
        snapshot.coverage.covered = ScreenSize::new(40.0, size.height);
        snapshot
    }

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
        let tile = TileOf {
            window,
            floating,
            server_order,
            worn: Worn::default(),
            radius: None,
        };
        let drawn = Drawn {
            snapshot: test_picture(to.size),
            clipped: false,
        };
        tile.flying((from, to), drawn, false)
    }

    fn layout(waits: Waits) -> Shape {
        Shape {
            kind: Kind::LayoutPass,
            start: Start::Coalesced,
            duration: Duration::from_millis(300),
            waits,
        }
    }

    /// What the coordinator is told of a flight.
    mod told {
        use super::*;

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
        fn a_later_passs_entrances_join_but_for_one_waiting_and_each_takes_the_latest_end() {
            let entrance = |window, to| PendingEntrance {
                window,
                to,
                floating: false,
                worn: Default::default(),
            };
            let (first, moved) = (rect(4.0, 32.0, 859.0, 1081.0), rect(867.0, 32.0, 859.0, 1081.0));
            let (joining, still) =
                (rect(1730.0, 32.0, 859.0, 1081.0), rect(0.0, 0.0, 400.0, 300.0));
            let mut running = flight();
            running.entrances.push(entrance(wid(1), on(first)));
            running.final_frames = vec![(wid(1), first), (wid(3), still)];

            running.take_ends(
                vec![entrance(wid(1), on(still)), entrance(wid(2), on(still))],
                vec![(wid(1), moved), (wid(2), joining)],
                Some(DISPLAY),
            );

            let entrances: Vec<_> = running.entrances.iter().map(|e| (e.window, e.to)).collect();
            assert_eq!(entrances, vec![(wid(1), on(moved)), (wid(2), on(joining))]);
            assert_eq!(running.final_frames, vec![
                (wid(1), moved),
                (wid(3), still),
                (wid(2), joining)
            ]);

            running.take_ends(vec![entrance(wid(4), on(still))], vec![(wid(4), first)], None);
            assert_eq!(
                running.entrances[2].to,
                on(still),
                "with no display, nothing is retargeted"
            );
            assert_eq!(running.final_frames[3], (wid(4), first));
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
            let park = rect(1727.0, 32.0, 859.0, 1081.0);
            let slot = rect(4.0, 32.0, 859.0, 1081.0);
            let stand_in = stand_in(slot.size.respace());
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

        /// Frame zero posed again is banded as the tiles band with the flight's focus, and the
        /// accents waiting for the motion still wait: the motion starting takes them, and the flight
        /// moves from then.
        #[test]
        fn frame_zero_posed_again_bands_its_tiles_and_leaves_the_accents_waiting() {
            use crate::animation::domain::motion::bounce::Bounce;
            let (mut running, _) = posed();
            running.focus = Some(wid(2));
            let right = OverlayVector::new(72.0, 0.0);
            assert_eq!(running.push(&Cue::Plain, right, 0.35, 10.0), None);
            let score = running.pose();
            assert_eq!(
                *score.banding(),
                band_plan(&running.plan, &running.tiles, Some(wid(2)))
            );
            assert!(!running.flown);
            let start = running.start_moving(0.35, 10.0);
            assert!(running.flown);
            let plays = Duration::from_secs_f64(0.35);
            let key = strip_key(&running);
            assert!(
                start.contains(&Edit::Add(Bounce::from_rest(right, plays).track(key, 10.0))),
                "the push waited through the pose"
            );
            assert_eq!(running.bank(), Edit::Bank(score.banding().clone()));
        }

        /// A repaint held for a corner of a leg in one movement is its tile's own as the motion
        /// starts: there is no corner for it to wait for.
        #[test]
        fn the_motion_starting_turns_a_corner_no_leg_has() {
            let (mut running, _) = posed();
            let repaint = test_picture(ScreenSize::new(600.0, 1081.0));
            running.tiles[0].from_corner = Some(repaint.clone());
            running.start_moving(0.35, 10.0);
            let tile = &running.tiles[0];
            assert!(tile.from_corner.is_none());
            assert_eq!(tile.snapshot.coverage, repaint.coverage);
        }

        /// A reserved entrance's tile joins frame zero's loose container before the motion, frame
        /// zero to be posed again, and once the flight moves is merged in as a pass of its own, the
        /// tiles banded again last.
        #[test]
        fn a_joining_entrance_is_laid_in_frame_zero_before_the_motion_and_merged_into_it_after() {
            let slot = rect(1730.0, 32.0, 859.0, 1081.0);
            let entering = |running: &mut RunningAnimation| {
                let mut tile = stacked(wid(3), entrance_from(slot), slot, Some(0), false);
                tile.focused = true;
                running.tiles.push(tile.clone());
                tile
            };
            let (mut running, _) = posed();
            let tile = entering(&mut running);
            assert_eq!(running.join(&tile, VIEWPORT, 0.3, 10.0), None);
            let loose = running.plan.position_of(plan::GroupKey::Loose);
            assert_eq!(
                running.plan.entrances.last(),
                Some(&(
                    wid(3),
                    plan::group_relative(entrance_from(slot), loose),
                    plan::group_relative(slot, loose)
                ))
            );
            assert!(running.plan.destination(wid(3)).unwrap().same_as(slot));

            let (mut running, _) = posed();
            running.start_moving(0.35, 10.0);
            let tile = entering(&mut running);
            let edits = running.join(&tile, VIEWPORT, 0.3, 10.1).expect("merged");
            assert!(running.plan.destination(wid(3)).unwrap().same_as(slot));
            assert!(
                edits.iter().any(|e| matches!(e, Edit::Add(t) if t.begin() == 10.1)),
                "{edits:?}"
            );
            assert_eq!(
                edits.last(),
                Some(&Edit::Bank(band_plan(
                    &running.plan,
                    &running.tiles,
                    running.focus
                )))
            );
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
            let (column, folded) =
                (rect(866.0, 35.0, 860.0, 1079.0), rect(3.0, 576.0, 1148.0, 538.0));
            assert!(tile::two_steps(column, folded).is_some());
            let mut running = flight();
            let mut tile = stacked(wid(1), on(column), on(folded), Some(0), false);
            tile.snapshot = stand_in(folded.size.respace());
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
                let mut cache: crate::pictures::domain::cache::PictureCache<Shot> =
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
}
