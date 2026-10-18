//! The overview on the flight engine's side: its lifecycle driven as the coordinator's is
//! (`lifecycle::overview`), its thumbs made from the pictures the service holds, the stand-in where
//! it holds none, and every change of its scene played on the overlay as the overview's
//! choreography has it (`animation::domain::choreography::overview`). While it holds the overlay
//! every flight asked for and every move of the overlay waits, and goes on at its lift.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use rini_core::ids::WindowId;
use rini_geometry::space::{OverlayRect, Respace, ScreenRect};
use rini_geometry::to_overlay_space;
use rini_runloop::run_loop::RepeatingTimer;
use tracing::debug;

use super::asks::on_screen;
use super::tiles::{Stacking, frames_of};
use super::{Event, FlightEngine};
use crate::animation::domain::choreography::overview::{
    Motion, Outline, Places, Press, at_rest, held, pose, pushes, screen_wide, toward,
};
use crate::animation::domain::motion::plan::on_overlay;
#[cfg(test)]
use crate::animation::domain::motion::score::Edit;
use crate::animation::domain::motion::score::Score;
use crate::animation::platform::overlay::OverlayTile;
use crate::animation::platform::renderer::Render;
use crate::decorations::domain::shape::outset;
use crate::decorations::domain::style::{Decoration, Ring, Worn};
use crate::overview::domain::changes::changes;
use crate::overview::domain::scene::Scene;
use crate::pictures::domain::schedule::Hold;
use crate::pictures::domain::watch::Moment;
use crate::pictures::platform::picture::Picture;
use crate::transitions::domain::drawn::Owed;
use crate::transitions::domain::flight::handover_report;
use crate::transitions::domain::lifecycle::overview::{Command, Event as Step};
use crate::transitions::domain::lifecycle::{Generation, frame_zero_pictures};
use crate::transitions::domain::request::{Cause, OverviewView};
use crate::transitions::domain::running::Drawn;
use crate::transitions::domain::timing::FRAME_INTERVAL;

/// What the overview draws, from its open to its lift.
#[derive(Default)]
pub(super) struct Shown {
    /// The latest view the reactor laid out.
    view: Option<OverviewView>,
    /// How long its zoom out and in run, and a press inside it.
    arc: Duration,
    press: Duration,
    /// Its score as the renderer was handed it.
    score: Score,
    tiles: Vec<OverlayTile>,
    /// Frame zero's pictures, until its tiles are made.
    frame_zero: HashMap<WindowId, Picture>,
    /// What the scenes since the last played named.
    presses: Vec<Press>,
    /// The outlines of its empty workspaces as last drawn.
    outlines: Vec<Outline>,
    /// Whether its thumbs' bands are drawn as wide on screen as at rest: while it is held.
    widened: bool,
    /// The scene it last played, which the next is logged against.
    played: Option<Scene>,
    /// What waits for its lift: every flight asked for and every move of the overlay, and an open
    /// on another display with what follows it, in order.
    held: Vec<Event>,
}

impl FlightEngine {
    /// The overview opened, its zoom out over `arc` and each press inside it over `press`: opened
    /// under a generation of its own, but for one waiting on a flight to lift, which takes one then,
    /// and one opened again as it closes, which keeps its own. A coordinator in its quiet settles
    /// first: the overview holds the service from here.
    pub(super) fn open_overview(
        &mut self,
        view: OverviewView,
        (arc, press): (Duration, Duration),
        cause: Cause,
    ) {
        self.take_request(cause);
        self.shown_overview.view = Some(view);
        self.shown_overview.arc = arc;
        self.shown_overview.press = press;
        let flying = self.flights.coordinator().is_flying();
        let generation = if flying || self.flights.overview().is_landing() {
            self.flights.overview().generation()
        } else {
            self.settle_quiet();
            self.shown_overview.frame_zero.clear();
            self.flights.reserve()
        };
        let rests = self.rests;
        self.drive_overview(Step::Open { generation, arc, flying, rests });
    }

    /// The scene laid out again, and what the presses that changed it named.
    pub(super) fn overview_scene(&mut self, view: OverviewView, presses: Vec<Press>) {
        self.shown_overview.view = Some(view);
        self.shown_overview.presses.extend(presses);
        self.drive_overview(Step::Scene);
    }

    /// The overview closed on `view`, the real layout it lands on.
    pub(super) fn close_overview(&mut self, view: OverviewView) {
        self.shown_overview.view = Some(view);
        self.drive_overview(Step::Close);
    }

    /// `event` held for the overview's lift while it holds the overlay: a flight, or a move of the
    /// overlay; an open as it closes on another display than the overlay's, which opens afresh
    /// there after the lift, and the scenes and close after it. Handed back otherwise, a flight
    /// beginning in its quiet taking the service from it.
    pub(super) fn wait_for_overview(&mut self, event: Event) -> Option<Event> {
        let waits = match event {
            Event::Animate { .. }
            | Event::AnimateSurface { .. }
            | Event::Bounce { .. }
            | Event::SetDisplay { .. } => true,
            Event::OverviewOpen { .. } => self.opens_elsewhere(),
            Event::OverviewScene { .. } | Event::OverviewClose { .. } => self.open_waits(),
            _ => false,
        };
        if !waits {
            return Some(event);
        }
        if self.flights.overview().holds_overlay() {
            self.shown_overview.held.push(event);
            return None;
        }
        if self.flights.overview().is_settling() && event.pressed().is_some() {
            self.drive_overview(Step::Overtaken);
        }
        Some(event)
    }

    /// Whether an open now, as it closes, is for another display than the one it is drawn on: the
    /// last move of the overlay it holds is to another display. Zoomed out again there, it would
    /// draw that display's scene on this one's overlay.
    fn opens_elsewhere(&self) -> bool {
        if !self.flights.overview().is_landing() {
            return false;
        }
        let moved_to = self.shown_overview.held.iter().rev().find_map(|event| match event {
            Event::SetDisplay { id, .. } => Some(*id),
            _ => None,
        });
        moved_to.is_some_and(|id| Some(id) != self.display_id)
    }

    /// Whether an open waits for the lift among what is held.
    fn open_waits(&self) -> bool {
        self.shown_overview
            .held
            .iter()
            .any(|event| matches!(event, Event::OverviewOpen { .. }))
    }

    /// The flight an overview waited for has lifted: it opens now, under a generation of its own.
    pub(super) fn open_overview_waiting(&mut self) {
        if !self.flights.overview().is_waiting() || self.flights.coordinator().is_flying() {
            return;
        }
        self.settle_quiet();
        let generation = self.flights.reserve();
        self.drive_overview(Step::FlightLifted(generation));
    }

    /// The quiet after the last lift ends at once, whoever's lift it was, armed or still waiting for
    /// the lift to be drawn: the overview holds the service and the report from here.
    fn settle_quiet(&mut self) {
        self.quiet = None;
        if self.flights.coordinator().is_settling() {
            let generation = self.flights.coordinator().generation();
            self.drive(crate::transitions::domain::lifecycle::Event::Quiet(generation));
        }
        if self.flights.overview().is_settling() {
            self.drive_overview(Step::Quiet(self.flights.overview().generation()));
        }
    }

    /// Tells the overview's lifecycle `event`, carries out what it says, and sends the writer the
    /// burst due.
    pub(super) fn drive_overview(&mut self, event: Step) {
        let now = self.now();
        let (commands, burst) = self.flights.drive_overview(event, now);
        for command in commands {
            self.carry_out_overview(command, now);
        }
        if let (Some(burst), Some(writer)) = (burst, &self.writer) {
            writer.send(burst);
        }
    }

    fn carry_out_overview(&mut self, command: Command, now: Instant) {
        match command {
            Command::Hold => {
                self.drawn.begun();
                self.pictures.hold(Hold::Transition);
            }
            Command::AskFrameZero(generation) => self.ask_overview_frame_zero(generation),
            Command::Show => self.show_overview(now),
            Command::Release => self.pictures.hold(Hold::Released),
            Command::Play => {
                let presses = std::mem::take(&mut self.shown_overview.presses);
                let press = self.shown_overview.press;
                self.move_overview(Target::Held, slide(press), (&presses, press), now);
            }
            Command::Exit => {
                self.hole_overview();
                let arc = self.shown_overview.arc;
                self.move_overview(Target::AtRest, arc_motion(arc), (&[], arc), now);
            }
            Command::Turn => {
                self.render(Render::Holes(Vec::new()));
                let arc = self.shown_overview.arc;
                self.move_overview(Target::Held, arc_motion(arc), (&[], arc), now);
            }
            Command::PlaceRest => {
                self.render(Render::PlaceRest { through: self.took });
            }
            Command::Lift(generation) => self.lift_overview(generation),
            Command::Settle => self.settle(),
            Command::Forget => self.shown_overview.forget(),
            Command::ReportUp => self.report(true),
            Command::ReportSettled => self.report(false),
            Command::Clock(on) => self.overview_clock(on),
            Command::Widen(on) => self.widen_overview(on),
            Command::Replay => {
                for event in std::mem::take(&mut self.shown_overview.held) {
                    self.tx.send(event);
                }
            }
        }
    }

    /// Frame zero of the windows the display shows, each wholly on it: the strip's, and the
    /// floating ones as the background behind it.
    fn ask_overview_frame_zero(&mut self, generation: Generation) {
        let (Some(view), Some((display, _))) = (&self.shown_overview.view, self.display) else {
            return;
        };
        let floats = |window: WindowId| {
            view.windows.iter().any(|info| info.window == window && info.floating)
        };
        let on = |frame: ScreenRect| frame_zero_pictures(frame, false, display);
        let (background, strip): (Vec<_>, Vec<_>) = view
            .shown
            .iter()
            .copied()
            .filter(|(_, frame)| on(*frame))
            .partition(|(window, _)| floats(*window));
        let behind: Vec<WindowId> = background.iter().map(|(window, _)| *window).collect();
        self.pictures.ask_on_screen(on_screen(
            self.tag(generation),
            Moment::FrameZero,
            &strip,
            &background,
            &behind,
        ));
    }

    /// Frame zero's pictures for `generation`, or a late one swapped onto its tiles: whether they
    /// were the overview's.
    pub(super) fn overview_frame_zero(
        &mut self,
        generation: Generation,
        pictures: &[(WindowId, Picture, ScreenRect)],
    ) -> bool {
        let overview = self.flights.overview();
        if overview.generation() != generation || !overview.holds_overlay() {
            return false;
        }
        if overview.is_shown() {
            for (window, ..) in pictures {
                self.overview_picture_taken(*window);
            }
            return true;
        }
        for (window, picture, _) in pictures {
            self.shown_overview.frame_zero.insert(*window, picture.clone());
        }
        self.drive_overview(Step::FrameZero(generation));
        true
    }

    /// The cache took a picture of `window`: onto its thumb, if the overview draws one.
    pub(super) fn overview_picture_taken(&mut self, window: WindowId) {
        if !self.flights.overview().is_shown() {
            return;
        }
        let Some(tile) = self.shown_overview.tiles.iter_mut().find(|tile| tile.window == window)
        else {
            return;
        };
        let Some(picture) = self.pictures.usable(window) else {
            return;
        };
        let clipped = picture.source.clips();
        tile.snapshot = picture.clone();
        tile.clipped = clipped;
        self.render(Render::Swap { window, picture, clipped });
    }

    /// The overlay dressed, frame zero posed from the screen as it is and shown, and the zoom out
    /// begun from it.
    fn show_overview(&mut self, now: Instant) {
        let Some(rest) = self.overview_places(Target::AtRest) else {
            return self.drive_overview(Step::Unavailable);
        };
        if let Some(at) = self.picture_display()
            && let Some((display, _)) = self.display
        {
            let dressing = self.pictures.dressing(&*self.window_server, at);
            self.render(Render::Dress {
                desktop: dressing.desktop,
                bar: dressing.bar,
                strip: dressing.strip.map(|strip| to_overlay_space(strip, display)),
            });
        }
        self.wear_overview(&rest);
        self.shown_overview.frame_zero.clear();
        self.shown_overview.presses.clear();
        self.shown_overview.score = pose(&rest);
        self.render(Render::Stage {
            score: self.shown_overview.score.clone(),
            tiles: self.shown_overview.tiles.clone(),
        });
        self.outline_overview(&rest);
        self.shown = self.render(Render::Show);
        let arc = self.shown_overview.arc;
        self.move_overview(Target::Held, arc_motion(arc), (&[], arc), now);
    }

    /// Every node from where it is drawn to where `target` has it, as `motion` moves it, the pushes
    /// among `presses` bounced over `pushing`, each thumb wearing what it wears there.
    fn move_overview(
        &mut self,
        target: Target,
        motion: Motion,
        (presses, pushing): (&[Press], Duration),
        now: Instant,
    ) {
        let Some(places) = self.overview_places(target) else {
            return;
        };
        let Some(scene) = self.shown_overview.view.as_ref().map(|view| view.scene.clone()) else {
            return;
        };
        if let Some(played) = self.shown_overview.played.replace(scene.clone()) {
            let changed = changes(&played, &scene);
            debug!(
                thumbs = changed.tiles.len(),
                crossed = changed.tiles.iter().filter(|tile| tile.holder_changed()).count(),
                rows_panned = changed.pans.len(),
                scrolled = changed.scroll,
                selection = ?changed.selection.map(|moved| moved.to),
                "overview moved"
            );
        }
        let worn_before: Vec<(WindowId, bool, Worn)> = self
            .shown_overview
            .tiles
            .iter()
            .map(|tile| (tile.window, tile.focused, tile.worn))
            .collect();
        self.wear_overview(&places);
        let at = self.media.at(now);
        let shown = &mut self.shown_overview;
        let mut edits = toward(&shown.score, &places, at, motion);
        edits.extend(pushes(&shown.score, &scene, presses, pushing.as_secs_f64(), at));
        for edit in &edits {
            shown.score.apply(edit);
        }
        let mut turned = Vec::new();
        for tile in &shown.tiles {
            let was = worn_before.iter().find(|(window, ..)| *window == tile.window);
            if let Some(&(_, focused, worn)) = was {
                if focused != tile.focused {
                    turned.push(Render::Focus {
                        window: tile.window,
                        focused: tile.focused,
                    });
                }
                if worn != tile.worn {
                    turned.push(Render::Wear {
                        window: tile.window,
                        worn: tile.worn,
                    });
                }
            }
        }
        if !edits.is_empty() {
            let tiles = shown.tiles.clone();
            self.render(Render::Play { edits, tiles });
        }
        for command in turned {
            self.render(command);
        }
        self.outline_overview(&places);
    }

    /// The outlines of `places`' empty workspaces, drawn when they changed.
    fn outline_overview(&mut self, places: &Places) {
        if self.shown_overview.outlines == places.outlines {
            return;
        }
        self.shown_overview.outlines = places.outlines.clone();
        self.draw_outlines();
    }

    /// The outlines as last laid out, ringed in the focus ring's band as wide as the thumbs' are.
    fn draw_outlines(&mut self) {
        let shown = &self.shown_overview;
        let ring = shown.view.as_ref().and_then(|view| {
            let ring = Decoration {
                ring: Some(view.outline?),
                ..Decoration::default()
            };
            shown.dressed(ring, view.scene.scale).ring
        });
        let outlines = shown.outlines.clone();
        self.render(Render::Outlines { outlines, ring });
    }

    /// Every thumb's bands, and the outlines', drawn as wide on screen as at rest while `on`, at
    /// their own width otherwise.
    fn widen_overview(&mut self, on: bool) {
        let shown = &mut self.shown_overview;
        let Some(view) = shown.view.as_ref() else { return };
        if shown.widened == on {
            return;
        }
        shown.widened = on;
        let mut rewear = Vec::new();
        for tile in &mut shown.tiles {
            let Some(info) = view.windows.iter().find(|info| info.window == tile.window) else {
                continue;
            };
            tile.worn.decoration = if on {
                screen_wide(info.decoration, view.scene.scale)
            } else {
                info.decoration
            };
            rewear.push(Render::Rewear {
                window: tile.window,
                worn: tile.worn,
            });
        }
        for command in rewear {
            self.render(command);
        }
        if !self.shown_overview.outlines.is_empty() {
            self.draw_outlines();
        }
    }

    /// Where `target` has every node, from the latest view: none with no view or no display.
    fn overview_places(&self, target: Target) -> Option<Places> {
        let view = self.shown_overview.view.as_ref()?;
        let (display, _) = self.display?;
        Some(match target {
            Target::Held => held(&view.scene),
            Target::AtRest => {
                let shown: Vec<(WindowId, OverlayRect)> = view
                    .shown
                    .iter()
                    .map(|(window, frame)| (*window, to_overlay_space(*frame, display)))
                    .collect();
                at_rest(&view.scene, &shown)
            }
        })
    }

    /// A thumb for every window `places` draws, made on the picture frame zero took, else the
    /// cache's, else the stand-in, and kept for one it drew already; the selected window's wearing
    /// the active ring and drawn focused, every other one dressed as on a workspace not shown.
    fn wear_overview(&mut self, places: &Places) {
        let Some(view) = self.shown_overview.view.as_ref() else {
            return;
        };
        let selected = view.scene.selected.window;
        let new: Vec<_> = places
            .tiles
            .iter()
            .filter(|(window, ..)| !self.shown_overview.tiles.iter().any(|t| t.window == *window))
            .filter_map(|(window, _, frame)| {
                let info = view.windows.iter().find(|info| info.window == *window)?;
                Some((*info, *frame))
            })
            .collect();
        let ids: Vec<_> = new.iter().map(|(info, _)| info.server_id).collect();
        let stacking = Stacking::read(&*self.window_server, &ids);
        let mut made = Vec::new();
        for (info, frame) in new {
            let rect = on_overlay(frame);
            let picture = self
                .shown_overview
                .frame_zero
                .get(&info.window)
                .cloned()
                .or_else(|| self.pictures.usable(info.window))
                .or_else(|| self.pictures.stand_in(rect.size.respace()));
            let Some(snapshot) = picture else { continue };
            let worn = Worn::beside(self.shown_overview.dressed(info.decoration, view.scene.scale));
            let tile = stacking.tile_of(info.window, info.server_id, info.floating, worn);
            let clipped = snapshot.source.clips();
            made.push(tile.flying((rect, rect), Drawn { snapshot, clipped }, false));
        }
        let shown = &mut self.shown_overview;
        shown
            .tiles
            .retain(|tile| places.tiles.iter().any(|(window, ..)| *window == tile.window));
        shown.tiles.extend(made);
        for tile in &mut shown.tiles {
            let on = selected == Some(tile.window);
            let decoration = tile.worn.decoration;
            tile.focused = on;
            tile.worn = if on {
                Worn {
                    decoration,
                    wears_ring: Ring::Active,
                    holds_focus: true,
                }
            } else {
                Worn::beside(decoration)
            };
        }
    }

    /// The overlay holed over each window the overview lands on, and over the band it wears there,
    /// so each keeps painting under it.
    fn hole_overview(&mut self) {
        let (Some(view), Some((display, scale))) = (&self.shown_overview.view, self.display) else {
            return;
        };
        let selected = view.scene.selected.window;
        let holes = view
            .shown
            .iter()
            .map(|(window, frame)| {
                let decoration = view
                    .windows
                    .iter()
                    .find(|info| info.window == *window)
                    .map(|info| info.decoration)
                    .unwrap_or_default();
                let band = outset(&decoration, selected == Some(*window), scale);
                (to_overlay_space(*frame, display), band)
            })
            .collect();
        self.render(Render::Holes(holes));
    }

    /// One frame of its clock: what a tick reads once it zooms back in, where its tiles are drawn
    /// and where the windows it lands on are.
    pub(super) fn overview_tick(&mut self) {
        let now = self.now();
        let (settled, landed) = if self.flights.overview().is_landing() {
            let at = self.media.at(now);
            (
                self.shown_overview.score.settled(at) && self.drawn.caught_up(),
                self.overview_landed(),
            )
        } else {
            (false, false)
        };
        self.drive_overview(Step::Tick { settled, landed });
    }

    /// Whether every window the overview lands on is where the window server has it.
    fn overview_landed(&self) -> bool {
        let (Some(view), Some((display, _))) = (&self.shown_overview.view, self.display) else {
            return true;
        };
        let windows: Vec<WindowId> = view.shown.iter().map(|(window, _)| *window).collect();
        let ids: Vec<_> = view
            .windows
            .iter()
            .filter(|info| windows.contains(&info.window))
            .map(|info| info.server_id)
            .collect();
        let by_server = frames_of(&*self.window_server, &ids);
        let real: HashMap<WindowId, ScreenRect> = view
            .windows
            .iter()
            .filter_map(|info| Some((info.window, *by_server.get(&info.server_id)?)))
            .collect();
        handover_report(&view.shown, &windows, &real, display).count_over == 0
    }

    /// Its overlay down over the real windows, its tiles let go, and its quiet once that is drawn.
    fn lift_overview(&mut self, generation: Generation) {
        self.hide(vec![Owed::Quiet(generation)]);
        self.shown_overview.forget();
    }

    /// Its frame clock started or stopped: a flight's, when one flies, is left alone.
    fn overview_clock(&mut self, on: bool) {
        if self.flights.coordinator().is_flying() {
            return;
        }
        if !on {
            self.frame_clock = None;
            return;
        }
        if self.frame_clock.is_none() {
            let tx = self.tx.clone();
            self.frame_clock = RepeatingTimer::every(FRAME_INTERVAL, move || {
                tx.send(Event::Tick);
            });
        }
    }

    /// The overview's score, for the tests.
    #[cfg(test)]
    pub(super) fn overview_score(&self) -> &Score { &self.shown_overview.score }

    /// What its tiles are drawn with, for the tests.
    #[cfg(test)]
    pub(super) fn overview_tiles(&self) -> &[OverlayTile] { &self.shown_overview.tiles }
}

impl Shown {
    /// What it drew let go: the next open makes its thumbs afresh, at their own width.
    fn forget(&mut self) {
        self.tiles.clear();
        self.score = Score::default();
        self.frame_zero.clear();
        self.presses.clear();
        self.outlines.clear();
        self.widened = false;
        self.played = None;
    }

    /// `decoration` as its thumb wears it now: as wide on screen as at rest while it is held.
    fn dressed(&self, decoration: Decoration, scale: f64) -> Decoration {
        if self.widened {
            screen_wide(decoration, scale)
        } else {
            decoration
        }
    }
}

/// Where a move of the overview goes: zoomed out, or zoomed in at rest on the shown workspace.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Target {
    Held,
    AtRest,
}

/// The zoom out or in over `arc`, a cut with none.
fn arc_motion(arc: Duration) -> Motion {
    if arc.is_zero() {
        Motion::Cut
    } else {
        Motion::Arc { seconds: arc.as_secs_f64() }
    }
}

/// A press over `press`, a cut with none.
fn slide(press: Duration) -> Motion {
    if press.is_zero() {
        Motion::Cut
    } else {
        Motion::Slide { seconds: press.as_secs_f64() }
    }
}

/// The edits a test reads back: what a `Play` handed the renderer.
#[cfg(test)]
pub(super) fn played(commands: &[Render]) -> Vec<&Edit> {
    commands
        .iter()
        .flat_map(|command| match command {
            Render::Play { edits, .. } => edits.iter().collect(),
            _ => Vec::new(),
        })
        .collect()
}
