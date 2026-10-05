//! The lifecycle every transition runs through, whatever moves in it: frame-zero pictures, the
//! overlay up, the real frames written, end-state pictures swapped in as the windows land, a press
//! merged in flight, the lift, and the quiet after it. Pure: every event carries its instant, and the
//! shell carries out the commands. See `docs/pictures-motion-transitions.md`.
//!
//! What a kind stages and how its plan merges stay with the kind and the motion; the coordinator sees
//! a [`StageSummary`] and decides only when.
use std::collections::HashSet;
use std::time::{Duration, Instant};

use rini_core::ids::WindowId;
use rini_geometry::space::{OverlayRect, ScreenRect, SurfacePoint};
use rini_geometry::{is_off_screen, to_overlay_space};
use serde::Serialize;

use crate::animation::domain::motion::tile::snap_to;
use crate::pictures::domain::watch::{Accept, CHANGE_EVERY, Client, END_SLACK, Tag, Why, near, wholly_on};

use super::request::SurfaceWindow;
use super::timing::{COALESCE_WINDOW, HOLD_CAP, LIFT_GRACE};

/// How long the overlay waits for its frame-zero pictures before showing what the cache holds.
pub const FRAME_ZERO_PATIENCE: Duration = Duration::from_millis(80);
/// How long the end windows are watched to land after their frames are written.
pub const LANDING_PATIENCE: Duration = Duration::from_millis(150);
/// The last end-state capture starts this long before the clock ends, so a slow capture still lands.
pub const FINAL_BEFORE_END: Duration = Duration::from_millis(110);
/// ... and never sooner than this after the motion starts.
pub const FINAL_NOT_BEFORE: Duration = Duration::from_millis(40);

/// One press, or one burst merged in flight. Every picture asked for carries it, so one taken for an
/// end its tile no longer has is never drawn.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub struct Generation(pub u64);

impl Generation {
    /// The generation a delivery under `tag` is for, when `client` is the one it was delivered to.
    pub fn for_client(tag: Tag, client: Client) -> Option<Generation> {
        (tag.client == client).then_some(Generation(tag.generation))
    }
}

/// What kind of change a transition is. The lifecycle is the same for all; each kind decides the few
/// things below.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// The strip scrolling to bring a window into view.
    Pan,
    /// A window moved along the strip.
    Move,
    /// A switch to another workspace in the stack.
    Switch,
    /// A layout pass: resizes, swaps, a window opening.
    LayoutPass,
    /// A push against an end: the surface gives and comes back; no window moves.
    Bounce,
}

impl Kind {
    /// The kind of strip movement a surface flight draws: a move when a swapped column crosses the
    /// surface, a switch when the viewport travels between workspaces, else a pan.
    pub fn of_surface(
        windows: &[SurfaceWindow],
        from_offset: SurfacePoint,
        to_offset: SurfacePoint,
    ) -> Kind {
        if windows.iter().any(|w| w.from.is_some()) {
            Kind::Move
        } else if from_offset.y != to_offset.y {
            Kind::Switch
        } else {
            Kind::Pan
        }
    }

    /// Whether the real windows are written to new frames.
    pub fn writes(self) -> bool {
        !matches!(self, Kind::Bounce)
    }

    /// Whether the end state is watched and recaptured as it lands. Nothing lands in a bounce.
    pub fn watches_end(self) -> bool {
        !matches!(self, Kind::Bounce)
    }
}

/// When the motion starts once the overlay is up.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Start {
    Immediate,
    /// After [`COALESCE_WINDOW`], for the reactor's follow-up passes.
    Coalesced,
}

/// One drawn window: drawn now, or once its first picture lands (a reserved entrance).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TileSummary {
    pub window: WindowId,
    pub start: ScreenRect,
    pub end: ScreenRect,
    pub floating: bool,
    /// A border tracing another window: drawn, never pictured on its own here.
    pub companion: bool,
}

impl TileSummary {
    fn moves(&self) -> bool {
        !near(self.start, self.end, 0.5)
    }
}

/// What a kind staged, as far as the lifecycle needs to know.
#[derive(Clone, Debug, PartialEq)]
pub struct StageSummary {
    pub kind: Kind,
    pub tiles: Vec<TileSummary>,
    /// Where the real windows go: every moving window, drawn or not.
    pub final_frames: Vec<(WindowId, ScreenRect)>,
    pub focus: Option<WindowId>,
    /// The window focus leaves, when focus moves. Its frame-zero picture still shows it focused if it
    /// was the key window (`key`).
    pub losing_focus: Option<WindowId>,
    /// The key window as the flight's first request went out, as far as the reactor had heard: what
    /// frame zero pictures drawn focused.
    pub key: Option<WindowId>,
    /// The focus target, when its tile flies its focused look (`parked_focus`).
    pub focused_look: Option<WindowId>,
    /// Windows this pass opens or grows, not painted at their new frames yet: watched until they
    /// repaint, and held for, overlay up, until the motion starts.
    pub awaiting: Vec<WindowId>,
    /// Windows this pass opens that fly from where macOS first showed them: watched until they repaint
    /// where they end, and never held for.
    pub repainting: Vec<WindowId>,
    pub start: Start,
    /// The motion's length: the press's own, and for a merge the score's when that is longer. A merge
    /// that moves anything restarts the clock with it.
    pub duration: Duration,
    /// The bounds of the display the overlay covers, the menu bar's band included: what its holes and
    /// snaps are drawn against.
    pub display: ScreenRect,
    /// Windows nothing shows through: measured opaque, no material.
    pub opaque: Vec<WindowId>,
}

/// Whether `window`'s tile is drawn focused until its turn: the window focus is on as the flight
/// begins, and the focus target only when focus is not moving to it. What frame zero pictures
/// of either shows the same, unless another application's panel had the keys (`frame_zero_look`). A
/// focus target flying its focused look (`StageSummary::parked_focus`) is drawn focused with it.
pub fn drawn_focused(window: WindowId, focus: Option<WindowId>, losing_focus: Option<WindowId>) -> bool {
    losing_focus == Some(window) || focus == Some(window) && losing_focus.is_none()
}

/// The window whose frame-zero picture is its focused look: the one focus leaves, if it was the key
/// window as the press went out. A click's report has made its target key before the request it
/// causes, and another application's panel taking the keys leaves the layout's window drawn unfocused.
pub fn frame_zero_look(losing_focus: Option<WindowId>, key: Option<WindowId>) -> Option<WindowId> {
    losing_focus.filter(|window| key == Some(*window))
}

/// The window focus leaves when focus moves to `focus`: `shown`, the one drawn focused now, if not that
/// one. A pass naming no focus moves none.
pub fn losing_focus(focus: Option<WindowId>, shown: Option<WindowId>) -> Option<WindowId> {
    focus.and(shown.filter(|window| Some(*window) != focus))
}

impl StageSummary {
    /// The focus target when it starts off the display, where frame zero cannot picture it: it flies
    /// its focused look, if the pictures service holds one that fits its tile.
    pub fn parked_focus(&self) -> Option<WindowId> {
        let window = self.focus?;
        (!wholly_inside(self.tile(window)?.start, self.display)).then_some(window)
    }

    /// Strip windows wholly on screen at their start: what frame zero must picture fresh.
    fn frame_zero_strip(&self) -> Vec<(WindowId, ScreenRect)> {
        self.tiles
            .iter()
            .filter(|t| !t.floating && !t.companion && wholly_inside(t.start, self.display))
            .map(|t| (t.window, t.start))
            .collect()
    }

    /// Floating windows that stay put, wholly on screen: what shows behind the strip.
    fn background(&self) -> Vec<(WindowId, ScreenRect)> {
        self.tiles
            .iter()
            .filter(|t| t.floating && !t.companion && !t.moves() && wholly_inside(t.start, self.display))
            .map(|t| (t.window, t.start))
            .collect()
    }

    /// The background windows seen at frame zero, and whether any was left for later: one wholly
    /// behind a single opaque strip window at its start cannot be seen until the strip moves.
    fn background_at_frame_zero(&self) -> (Vec<(WindowId, ScreenRect)>, bool) {
        let strip = self.frame_zero_strip();
        let (seen, hidden): (Vec<_>, Vec<_>) = self.background().into_iter().partition(|(_, frame)| {
            !strip.iter().any(|(w, s)| wholly_on(*frame, *s) && self.opaque.contains(w))
        });
        (seen, !hidden.is_empty())
    }

    /// The floating windows, composited behind the strip in its captures so a material in the strip
    /// blurs them.
    fn behind(&self) -> Vec<WindowId> {
        self.tiles.iter().filter(|t| t.floating && !t.companion).map(|t| t.window).collect()
    }

    /// Strip windows that end wholly on screen: whose landing is watched and whose tiles get holes.
    fn end_windows(&self) -> Vec<(WindowId, ScreenRect)> {
        self.final_frames
            .iter()
            .filter(|(w, f)| {
                wholly_inside(*f, self.display)
                    && self.tiles.iter().any(|t| t.window == *w && !t.floating && !t.companion)
            })
            .copied()
            .collect()
    }

    /// Of `awaiting`, the windows that end wholly on screen, floating or not: what a watch can see
    /// repaint.
    fn awaited_ends(&self, awaiting: &[WindowId]) -> Vec<(WindowId, ScreenRect)> {
        self.tiles
            .iter()
            .filter(|t| awaiting.contains(&t.window) && !t.companion && wholly_inside(t.end, self.display))
            .map(|t| (t.window, t.end))
            .collect()
    }

    fn tile(&self, window: WindowId) -> Option<&TileSummary> {
        self.tiles.iter().find(|t| t.window == window)
    }

    fn ends(&self) -> impl Iterator<Item = (WindowId, ScreenRect)> + '_ {
        self.tiles.iter().map(|t| (t.window, t.end))
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    /// A fresh transition. Arrives before the stage is composed, so frame zero starts first.
    Begin(StageSummary),
    /// A pass absorbed while a transition runs; its plan is already merged and the renderer retargeted.
    /// `moved` when the merge moved anything, which restarts the clock.
    Merge { stage: StageSummary, moved: bool },
    /// The frame-zero pictures arrived (or the service has nothing more to give).
    FrameZero(Generation),
    /// One picture, taken with the window at `frame`: an end-state one, or one of frame zero's that
    /// came after the overlay went up without it. Frame zero's in time come as `FrameZero`.
    Picture { generation: Generation, window: WindowId, frame: ScreenRect, why: Why },
    /// The end window watched under `generation` whose picture will not come: it was not where it ends
    /// by the watch's deadline, it never settled, or the cache refused its capture. For the window
    /// losing focus, its turn will not be seen either.
    Lost { generation: Generation, window: WindowId },
    /// The score plays this much longer from now: composed as the motion starts, or added to by a
    /// push or a merge that moves nothing. The clock runs on to cover it.
    Extend(Duration),
    /// The frame clock. `settled`: every tile is drawn where it ends. `landed`: the real windows are where
    /// they were written.
    Tick { settled: bool, landed: bool },
    /// The quiet after a lift ran out. Carries the generation that lifted, so a stale one is ignored.
    Quiet(Generation),
    /// The shell could not stage or show the transition: no overlay, no display.
    Abandon,
    /// The key window changed, which turned these windows' looks, each at the frame it is at: the one
    /// that became key and the one that stopped being.
    LooksTurned(Vec<(WindowId, ScreenRect)>),
}

/// What the shell carries out.
#[derive(Clone, Debug, PartialEq)]
pub enum Command {
    /// Hold the pictures service: the overlay is going up, and only the captures it waits for start.
    Hold,
    /// The overlay lifted: the bar is pictured again, and the rest of the service's work still waits.
    Settle,
    /// The quiet after the lift ran out, or the transition was abandoned: nothing holds the service.
    Release,
    /// Fresh pictures of the windows on screen at their start, and the background seen there, with the
    /// floating windows composited behind the strip.
    AskFrameZero {
        generation: Generation,
        strip: Vec<(WindowId, ScreenRect)>,
        background: Vec<(WindowId, ScreenRect)>,
        behind: Vec<WindowId>,
    },
    /// Overlay up, at frame zero.
    Show,
    Write(Vec<(WindowId, ScreenRect)>),
    /// A point of the overlay left clear over each window that ends on screen, so the window server
    /// counts it visible and the app keeps painting.
    Holes(Vec<OverlayRect>),
    /// Watch the end state as `EndWatch` says, in place of whatever was watched before.
    Watch(EndWatch),
    StopWatches,
    StartMoving,
    /// One more capture of every end window, before the lift.
    AskFinal { generation: Generation, windows: Vec<(WindowId, ScreenRect)>, behind: Vec<WindowId> },
    /// Draw the delivered picture on its window's tile.
    Swap { generation: Generation, window: WindowId },
    /// Move the tile onto its window's real frame.
    Snap { window: WindowId, frame: OverlayRect },
    /// The picture just swapped in shows the window focused: keep it as its focused look.
    KeepFocusedLook { generation: Generation, window: WindowId },
    /// Draw `window`'s tile focused or not, its ring and its shadow, as its turned picture comes in:
    /// the window focus leaves unfocused, and the focus target focused, which is focus handing over.
    /// Until then the tiles are drawn focused where frame zero pictured focus: on the window focus
    /// leaves, and on the target only when it flies its focused look (`StageSummary::parked_focus`).
    Focus { window: WindowId, focused: bool },
    /// Overlay down. The quiet that follows ends with `Event::Quiet(generation)`.
    Lift { generation: Generation },
    /// Pictures for the next transition of every window the flight drew, at the frame each ended at.
    Warm(Vec<(WindowId, ScreenRect)>),
    /// Fresh pictures of windows whose look turned with the key window, whatever the cache holds of
    /// them: a warm skips a picture that fits, and a focus change never changes a size.
    Refresh(Vec<(WindowId, ScreenRect)>),
    /// The overlay went up for a flight not already reported.
    ReportUp,
    ReportSettled,
}

/// The end state to watch, everything delivered under `generation`.
#[derive(Clone, Debug, PartialEq)]
pub struct EndWatch {
    pub generation: Generation,
    /// End windows captured once they are where they end (`Settle::AsSoonAsThere`)...
    pub landing: Vec<(WindowId, ScreenRect)>,
    /// ... and end windows a pass opened or grew, captured once they have repainted there
    /// (`Settle::Repainted`).
    pub awaiting: Vec<(WindowId, ScreenRect)>,
    /// A window of `landing` not there by then is lost...
    pub landing_until: Instant,
    /// ... and one of `awaiting`, when a hold for it runs out.
    pub awaiting_until: Instant,
    /// The focus target and where it ends, watched until its look changes: only while focus is moving
    /// to it and its focused look is still to come, never for a window `awaiting` watches, and never
    /// for one ending off the display.
    pub focus: Option<(WindowId, ScreenRect)>,
    /// The window focus leaves and where it ends, watched with `focus` until its own look changes: only
    /// while `focus` is watched and its own turn is still to come, and only for one ending wholly on
    /// the display.
    pub losing_focus: Option<(WindowId, ScreenRect)>,
    /// Floating windows that stay put, kept fresh once `focus`'s own change is in, if it is watched...
    pub background: Vec<(WindowId, ScreenRect)>,
    /// ... except one capture this soon when they were left out of frame zero.
    pub background_initial: Option<Instant>,
    /// The floating windows, composited behind the strip in its captures.
    pub behind: Vec<WindowId>,
}

#[derive(Clone, Debug, PartialEq)]
enum Phase {
    /// Waiting for the frame-zero pictures, overlay not up.
    Preparing { deadline: Instant },
    /// Overlay up, waiting for the follow-up passes.
    Coalescing { until: Instant },
    /// Overlay up, waiting for the awaited windows' first pictures, or to hear they will not come.
    Holding { deadline: Instant },
    Playing,
    /// The clock is done; waiting for the tiles and the windows to be where they end.
    Landing,
}

#[derive(Clone, Debug, PartialEq)]
struct Flight {
    generation: Generation,
    /// The generation it began under, which frame zero is asked and delivered under.
    began: Generation,
    stage: StageSummary,
    phase: Phase,
    /// When the motion started, and how long it runs from then.
    clock: Option<(Instant, Duration)>,
    /// The background was left out of frame zero.
    background_later: bool,
    /// Every window frame zero was asked for, where it was expected...
    asked: Vec<(WindowId, ScreenRect)>,
    /// ... and how many of those asks have not delivered yet.
    frame_zero_owed: usize,
    /// The focus target is drawn focused: focus has handed over.
    focus_in: bool,
    /// The window this flight drew unfocused, until it is drawn focused again, handed focus or flying
    /// its focused look: the one focus leaves, with its own turned picture or just before the
    /// target's. A press moving focus back to it finds it drawn unfocused, so focus moves to it as to
    /// any other window.
    unfocused: Option<WindowId>,
    /// The window focus leaves whose turn the pictures service will not see, until focus moves again:
    /// what the target's changed picture waits for no longer.
    unseen: Option<WindowId>,
    /// The focus target's changed picture, not swapped while the window losing focus is watched to
    /// turn first: under its generation, at its frame, since when. Swapped at that turn, or
    /// `CHANGE_EVERY` after.
    held_change: Option<(Generation, ScreenRect, Instant)>,
    /// The focus target's latest landing, not swapped over the focused look its tile flies until focus
    /// hands over: under its generation, at its frame.
    held_back: Option<(Generation, ScreenRect)>,
    final_asked: bool,
    snapped: HashSet<WindowId>,
    /// Where each window was last pictured at its end this generation: what its tile snaps onto.
    landed: Vec<(WindowId, ScreenRect)>,
    /// The generation since which each tile has ended where it ends: a picture delivered under an
    /// earlier one was taken for another end, and one under it or later still holds, whatever merged
    /// since it was taken.
    end_since: Vec<(WindowId, Generation)>,
    /// Every window a pass of this flight opened or grew: watched until it repaints.
    awaiting: Vec<WindowId>,
    /// Of `awaiting`, the windows flying from where macOS showed them, which nothing holds for.
    repainting: Vec<WindowId>,
    /// The awaited windows the overlay still holds for before the motion starts.
    held: Vec<WindowId>,
    /// Every window this flight drew, across its merged passes, at the frame it last ended at: what
    /// the quiet warms.
    animated: Vec<(WindowId, ScreenRect)>,
    /// Every window whose look the key window turned while the flight ran that no picture since has
    /// shown: what the quiet pictures afresh. The window losing focus that ends clipped or parked,
    /// and either end whose change no capture saw, are never pictured in flight.
    turned: Vec<(WindowId, ScreenRect)>,
}

impl Flight {
    /// Whether a picture of `window` delivered under `generation`, taken at `frame`, may be drawn on its
    /// tile: taken for the end the tile has now, within `END_SLACK` of it.
    fn takes(&self, generation: Generation, window: WindowId, frame: ScreenRect) -> bool {
        self.end_since.iter().any(|(w, since)| *w == window && generation >= *since)
            && self.stage.tile(window).is_some_and(|tile| near(frame, tile.end, END_SLACK))
    }

    /// Focus handed over to the target: the window left drawn unfocused first, if its own turn has
    /// not, then the target focused; with the landing held back from over its focused look, if one
    /// still fits, swapped in.
    fn hand_over(&mut self, to: WindowId, out: &mut Vec<Command>) {
        self.focus_in = true;
        self.unfocus_losing(out);
        if self.unfocused == Some(to) {
            self.unfocused = None;
        }
        if let Some((generation, frame)) = self.held_back.take()
            && self.takes(generation, to, frame)
        {
            out.push(Command::Swap { generation, window: to });
        }
        out.push(Command::Focus { window: to, focused: true });
    }

    /// The window focus leaves drawn unfocused, once.
    fn unfocus_losing(&mut self, out: &mut Vec<Command>) {
        if let Some(window) = self.stage.losing_focus
            && self.unfocused != Some(window)
        {
            self.unfocused = Some(window);
            out.push(Command::Focus { window, focused: false });
        }
    }

    /// The target's changed picture held for the window losing focus, swapped in and kept as its
    /// focused look, and focus handed over, if it still fits. One that no longer does is dropped: its
    /// ring turns with the next picture of it that fits, or at the last capture's ask.
    fn swap_held_change(&mut self, out: &mut Vec<Command>) {
        let (Some((generation, frame, _)), Some(to)) = (self.held_change.take(), self.stage.focus) else {
            return;
        };
        if !self.takes(generation, to, frame) {
            return;
        }
        self.unfocus_losing(out);
        out.push(Command::Swap { generation, window: to });
        out.push(Command::KeepFocusedLook { generation, window: to });
        self.held_back = None;
        self.hand_over(to, out);
    }

    /// Whether focus moves in this flight: away from a window drawn focused, or back to one drawn
    /// unfocused.
    fn focus_moving(&self) -> bool {
        self.stage.losing_focus.is_some() || self.unfocused.is_some() && self.unfocused == self.stage.focus
    }

    /// The windows watched until they repaint, and the end windows watched only until they are there.
    fn end_windows(&self) -> (Vec<(WindowId, ScreenRect)>, Vec<(WindowId, ScreenRect)>) {
        let landing = self.stage.end_windows().into_iter().filter(|(w, _)| !self.awaiting.contains(w)).collect();
        (self.stage.awaited_ends(&self.awaiting), landing)
    }

    /// The awaited windows the overlay can hold for: those a watch can see repaint, but for the ones
    /// flying from where macOS showed them.
    fn holdable(&self) -> Vec<WindowId> {
        self.end_windows().0.into_iter().map(|(w, _)| w).filter(|w| !self.repainting.contains(w)).collect()
    }

    /// Snaps `window`'s tile onto `frame` once per generation, once the motion has started: until
    /// then its container stands where frame zero put it, and the start would move it again.
    fn snap(&mut self, window: WindowId, frame: ScreenRect, out: &mut Vec<Command>) {
        if self.clock.is_none() {
            return;
        }
        let Some(tile) = self.stage.tile(window) else { return };
        if let Some(frame) = snap_to(tile.end, frame)
            && self.snapped.insert(window)
        {
            out.push(Command::Snap { window, frame: to_overlay_space(frame, self.stage.display) });
        }
    }

    /// Every tile whose window was pictured where it still ends, snapped again: a merge or the start
    /// can have placed it back on its model. A window no longer ending there is forgotten.
    fn snap_landed(&mut self, out: &mut Vec<Command>) {
        let stage = &self.stage;
        self.landed.retain(|(w, frame)| stage.tile(*w).is_some_and(|t| near(*frame, t.end, END_SLACK)));
        for (window, frame) in self.landed.clone() {
            self.snap(window, frame, out);
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
enum State {
    #[default]
    Idle,
    Flying(Box<Flight>),
    /// `owed` is warmed at the quiet, and `turned` pictured afresh then (`Flight::turned`).
    Settling { generation: Generation, owed: Vec<(WindowId, ScreenRect)>, turned: Vec<(WindowId, ScreenRect)> },
}

/// The lifecycle. One per overlay.
#[derive(Clone, Debug, Default)]
pub struct Coordinator {
    state: State,
    generation: Generation,
}

impl Coordinator {
    pub fn generation(&self) -> Generation {
        self.generation
    }

    /// Whether a transition is running: a pass now merges into it rather than starting one.
    pub fn is_flying(&self) -> bool {
        matches!(self.state, State::Flying(_))
    }

    /// Whether the overlay is up.
    pub fn is_shown(&self) -> bool {
        matches!(&self.state, State::Flying(f) if !matches!(f.phase, Phase::Preparing { .. }))
    }

    /// Whether the motion clock runs.
    pub fn is_moving(&self) -> bool {
        matches!(&self.state, State::Flying(f) if f.clock.is_some())
    }

    /// What is left of the motion's clock at `now`: what a tile joining it travels for. `None` before
    /// it starts.
    pub fn remaining(&self, now: Instant) -> Option<Duration> {
        let State::Flying(f) = &self.state else { return None };
        let (started, duration) = f.clock?;
        Some((started + duration).saturating_duration_since(now))
    }

    /// Whether a tick at `now` reads where the tiles are drawn and where the windows are: only once
    /// the clock is done does the lift ask.
    pub fn wants_landing(&self, now: Instant) -> bool {
        self.remaining(now).is_some_and(|left| left.is_zero())
    }

    pub fn on(&mut self, event: Event, now: Instant) -> Vec<Command> {
        let mut out = Vec::new();
        match event {
            Event::Begin(stage) => self.begin(stage, now, &mut out),
            Event::Merge { stage, moved } => self.merge(stage, moved, now, &mut out),
            Event::FrameZero(generation) => {
                if let State::Flying(f) = &mut self.state
                    && f.began == generation
                    && matches!(f.phase, Phase::Preparing { .. })
                {
                    f.frame_zero_owed = f.frame_zero_owed.saturating_sub(1);
                    if f.frame_zero_owed > 0 {
                        return out;
                    }
                    if let Some(window) = frame_zero_look(f.stage.losing_focus, f.stage.key) {
                        out.push(Command::KeepFocusedLook { generation, window });
                    }
                    show(f, now, &mut out);
                }
            }
            Event::Picture { generation, window, frame, why } => self.picture(generation, window, frame, why, now, &mut out),
            // A merge's retarget watches the window again, or reports the window losing focus given up on
            // lost again under the new generation: one lost under the generation before is dropped.
            Event::Lost { generation, window } => {
                if let State::Flying(f) = &mut self.state
                    && f.generation == generation
                {
                    if f.stage.losing_focus == Some(window) {
                        f.unseen = Some(window);
                        f.swap_held_change(&mut out);
                    }
                    hold_no_longer(f, window, now, &mut out);
                }
            }
            Event::Extend(needs) => {
                if let State::Flying(f) = &mut self.state
                    && let Some((started, duration)) = &mut f.clock
                {
                    *duration = (*duration).max(now - *started + needs);
                    if f.phase == Phase::Landing {
                        f.phase = Phase::Playing;
                    }
                }
            }
            Event::Tick { settled, landed } => self.tick(settled, landed, now, &mut out),
            Event::Abandon => {
                // Whatever never showed still has to be placed, and the hold released.
                if let State::Flying(f) = std::mem::take(&mut self.state) {
                    if f.stage.kind.writes() && !f.stage.final_frames.is_empty() {
                        out.push(Command::Write(f.stage.final_frames.clone()));
                    }
                    out.push(Command::StopWatches);
                    out.push(Command::Release);
                    out.push(Command::Warm(f.animated));
                    refresh(f.turned, &mut out);
                    out.push(Command::ReportSettled);
                }
            }
            Event::Quiet(generation) => {
                if let State::Settling { generation: g, owed, turned } = &mut self.state
                    && *g == generation
                {
                    out.push(Command::Release);
                    out.push(Command::Warm(std::mem::take(owed)));
                    refresh(std::mem::take(turned), &mut out);
                    out.push(Command::ReportSettled);
                    self.state = State::Idle;
                }
            }
            // At rest nothing else will picture them; a flight pictures the ends it lands and swaps,
            // and its quiet the rest.
            Event::LooksTurned(windows) => match &mut self.state {
                State::Idle => refresh(windows, &mut out),
                State::Flying(f) => extend_latest(&mut f.turned, windows.into_iter()),
                State::Settling { turned, .. } => extend_latest(turned, windows.into_iter()),
            },
        }
        out
    }

    fn begin(&mut self, stage: StageSummary, now: Instant, out: &mut Vec<Command>) {
        // A press inside the quiet carries the work owed to the next quiet, and the flight already
        // reported stays the one reported.
        let (owed, turned, reported) = match std::mem::take(&mut self.state) {
            State::Settling { owed, turned, .. } => (owed, turned, true),
            State::Idle => (Vec::new(), Vec::new(), false),
            flying @ State::Flying(_) => {
                self.state = flying;
                let moved = true;
                self.merge(stage, moved, now, out);
                return;
            }
        };
        self.generation.0 += 1;
        let generation = self.generation;
        let (background, background_later) = stage.background_at_frame_zero();
        let strip = stage.frame_zero_strip();
        let asked = strip.iter().chain(&background).copied().collect();
        out.push(Command::Hold);
        out.push(Command::AskFrameZero { generation, strip, background, behind: stage.behind() });
        let mut animated = owed;
        extend_latest(&mut animated, stage.ends());
        let mut awaiting = stage.awaiting.clone();
        extend_unique(&mut awaiting, stage.repainting.iter().copied());
        let end_since = ended_since(&[], None, &stage, generation);
        self.state = State::Flying(Box::new(Flight {
            generation,
            began: generation,
            awaiting,
            repainting: stage.repainting.clone(),
            stage,
            phase: Phase::Preparing { deadline: now + FRAME_ZERO_PATIENCE },
            clock: None,
            background_later,
            asked,
            frame_zero_owed: 1,
            focus_in: false,
            unfocused: None,
            unseen: None,
            held_change: None,
            held_back: None,
            final_asked: false,
            snapped: HashSet::new(),
            landed: Vec::new(),
            end_since,
            held: Vec::new(),
            animated,
            turned,
        }));
        if !reported {
            out.push(Command::ReportUp);
        }
    }

    fn merge(&mut self, stage: StageSummary, moved: bool, now: Instant, out: &mut Vec<Command>) {
        let State::Flying(f) = &mut self.state else {
            self.begin(stage, now, out);
            return;
        };
        extend_latest(&mut f.animated, stage.ends());
        extend_unique(&mut f.awaiting, stage.awaiting.iter().chain(&stage.repainting).copied());
        extend_unique(&mut f.repainting, stage.repainting.iter().copied());
        let changed = changed_frames(&f.stage.final_frames, &stage.final_frames);
        let focus_moved = f.stage.focus != stage.focus;
        let newcomers = stage.awaiting.clone();
        let before = std::mem::replace(&mut f.stage, stage);
        if matches!(f.phase, Phase::Preparing { .. }) {
            // Nothing is shown or written yet: the show takes the merged stage as it is, and waits for
            // frame zero to picture what the merge brought on screen too.
            f.end_since = ended_since(&f.end_since, Some(&before), &f.stage, f.generation);
            ask_frame_zero_again(f, out);
            return;
        }
        self.generation.0 += 1;
        f.generation = self.generation;
        f.end_since = ended_since(&f.end_since, Some(&before), &f.stage, f.generation);
        f.snapped.clear();
        f.final_asked = false;
        if focus_moved {
            f.focus_in = false;
            f.held_back = None;
            f.held_change = None;
            f.unseen = None;
            if f.stage.focused_look.is_some() && f.stage.focused_look == f.unfocused {
                f.unfocused = None;
            }
        }
        let holdable = f.holdable();
        // Until the motion starts, a newcomer is held for too; once moving, it joins as it lands.
        if f.clock.is_none() {
            extend_unique(&mut f.held, newcomers.into_iter().filter(|w| holdable.contains(w)));
        }
        if f.stage.kind.writes() && !changed.is_empty() {
            out.push(Command::Write(changed));
        }
        if moved && let Some((started, duration)) = &mut f.clock {
            *started = now;
            *duration = f.stage.duration;
            f.phase = Phase::Playing;
        }
        out.push(Command::Holes(holes(f)));
        if f.stage.kind.watches_end() {
            out.push(Command::Watch(end_watch(f, now)));
        }
        for window in f.held.clone().into_iter().filter(|w| !holdable.contains(w)) {
            hold_no_longer(f, window, now, out);
        }
        f.snap_landed(out);
    }

    fn picture(&mut self, generation: Generation, window: WindowId, frame: ScreenRect, why: Why, now: Instant, out: &mut Vec<Command>) {
        let State::Flying(f) = &mut self.state else { return };
        // Every delivery is in the cache, so the window's look now is too; frame zero's was taken at
        // the press, before any turn.
        if why != Why::FrameZero {
            f.turned.retain(|(w, _)| *w != window);
        }
        let Some(tile) = f.stage.tile(window) else { return };
        if why == Why::FrameZero {
            // Taken at the press, before any picture of the end: the cache's picture it replaces is
            // older, wherever the tile ends now.
            if generation == f.began && !matches!(f.phase, Phase::Preparing { .. }) && near(frame, tile.start, END_SLACK) {
                out.push(Command::Swap { generation, window });
                if frame_zero_look(f.stage.losing_focus, f.stage.key) == Some(window) {
                    out.push(Command::KeepFocusedLook { generation, window });
                }
            }
            return;
        }
        if !f.takes(generation, window, frame) {
            return;
        }
        let target = f.stage.focus == Some(window);
        // The focus target's look changing is the handover; one opened or grown shows it repainted;
        // and the last capture before the lift shows it as it lands, if its change watch ran out first.
        let handover = target
            && !f.focus_in
            && (matches!(why, Why::FocusChanged | Why::Final) || f.awaiting.contains(&window));
        // macOS turns the window focus leaves first: the target's change waits for it a little.
        let held = handover && why == Why::FocusChanged && watched_losing(f).is_some();
        // A landing is often taken before the window repaints focused: the focused look it flies
        // stays until focus hands over.
        let over_look = target
            && f.stage.focused_look == Some(window)
            && !f.focus_in
            && !f.awaiting.contains(&window)
            && why == Why::Landed;
        if held {
            let since = f.held_change.map_or(now, |(_, _, since)| since);
            f.held_change = Some((generation, frame, since));
        } else if over_look {
            f.held_back = Some((generation, frame));
        } else {
            if handover {
                f.unfocus_losing(out);
            }
            out.push(Command::Swap { generation, window });
        }
        extend_latest(&mut f.landed, [(window, frame)].into_iter());
        f.snap(window, frame, out);
        if target && why == Why::FocusChanged && !held {
            out.push(Command::KeepFocusedLook { generation, window });
        }
        if handover && !held {
            f.held_back = None;
            f.hand_over(window, out);
        }
        if f.stage.losing_focus == Some(window) && why == Why::FocusChanged {
            f.unfocus_losing(out);
            f.swap_held_change(out);
        }
        hold_no_longer(f, window, now, out);
    }

    fn tick(&mut self, settled: bool, landed: bool, now: Instant, out: &mut Vec<Command>) {
        let State::Flying(f) = &mut self.state else { return };
        if f.held_change.is_some_and(|(_, _, since)| now >= since + CHANGE_EVERY) {
            f.swap_held_change(out);
        }
        match f.phase.clone() {
            Phase::Preparing { deadline } => {
                if now >= deadline {
                    show(f, now, out);
                }
            }
            Phase::Coalescing { until } => {
                if now >= until {
                    hold_or_move(f, now, out);
                }
            }
            Phase::Holding { deadline } => {
                if now >= deadline {
                    start_moving(f, now, out);
                }
            }
            Phase::Playing | Phase::Landing => {
                let Some((started, duration)) = f.clock else { return };
                let end = started + duration;
                if !f.final_asked && f.stage.kind.watches_end() && now >= final_at(started, duration) {
                    f.final_asked = true;
                    out.push(Command::AskFinal {
                        generation: f.generation,
                        windows: f.stage.end_windows(),
                        behind: f.stage.behind(),
                    });
                    // Nothing showed the target's look change, or it looked focused before anything
                    // watched it: it is reported focused, and turning at the lift is the flicker.
                    f.swap_held_change(out);
                    if !f.focus_in
                        && f.focus_moving()
                        && let Some(to) = f.stage.focus.filter(|w| f.stage.tile(*w).is_some())
                    {
                        f.hand_over(to, out);
                    }
                }
                if now < end {
                    return;
                }
                f.phase = Phase::Landing;
                let overdue = now >= end + LIFT_GRACE;
                if (settled && landed) || overdue {
                    let generation = f.generation;
                    let owed = std::mem::take(&mut f.animated);
                    let turned = std::mem::take(&mut f.turned);
                    out.push(Command::StopWatches);
                    out.push(Command::Lift { generation });
                    out.push(Command::Settle);
                    self.state = State::Settling { generation, owed, turned };
                }
            }
        }
    }
}

/// Frame zero asked again for the windows a merge brought where the tiles start, on screen, that no ask
/// expected there: the show waits for that ask too, up to the patience the begin set.
fn ask_frame_zero_again(f: &mut Flight, out: &mut Vec<Command>) {
    let display = f.stage.display;
    let unasked = |windows: Vec<(WindowId, ScreenRect)>, asked: &[(WindowId, ScreenRect)]| -> Vec<(WindowId, ScreenRect)> {
        windows
            .into_iter()
            .filter(|(w, start)| {
                !asked.iter().any(|(a, at)| a == w && Accept::Origin { at: at.origin }.accepts(*start, display))
            })
            .collect()
    };
    let strip = unasked(f.stage.frame_zero_strip(), &f.asked);
    let background = unasked(f.stage.background_at_frame_zero().0, &f.asked);
    if !strip.is_empty() || !background.is_empty() {
        f.asked.extend(strip.iter().chain(&background).copied());
        f.frame_zero_owed += 1;
        out.push(Command::AskFrameZero { generation: f.began, strip, background, behind: f.stage.behind() });
    }
    let asked = &f.asked;
    f.background_later = f.stage.background().iter().any(|(w, _)| !asked.iter().any(|(a, _)| a == w));
}

/// The overlay goes up: the real windows are written behind it, and the end state is watched.
fn show(f: &mut Flight, now: Instant, out: &mut Vec<Command>) {
    out.push(Command::Show);
    if f.stage.kind.writes() && !f.stage.final_frames.is_empty() {
        out.push(Command::Write(f.stage.final_frames.clone()));
    }
    out.push(Command::Holes(holes(f)));
    if f.stage.kind.watches_end() {
        out.push(Command::Watch(end_watch(f, now)));
    }
    f.held = f.holdable();
    if f.stage.start == Start::Coalesced && f.held.is_empty() {
        f.phase = Phase::Coalescing { until: now + COALESCE_WINDOW };
    } else {
        hold_or_move(f, now, out);
    }
}

fn refresh(turned: Vec<(WindowId, ScreenRect)>, out: &mut Vec<Command>) {
    if !turned.is_empty() {
        out.push(Command::Refresh(turned));
    }
}

/// Holds for the awaited windows, up to `HOLD_CAP`, or starts the motion when there are none.
fn hold_or_move(f: &mut Flight, now: Instant, out: &mut Vec<Command>) {
    if f.held.is_empty() {
        start_moving(f, now, out);
    } else {
        f.phase = Phase::Holding { deadline: now + HOLD_CAP };
    }
}

/// `window` is held for no longer, its picture in or never to come: the motion starts once the hold
/// waits for nothing.
fn hold_no_longer(f: &mut Flight, window: WindowId, now: Instant, out: &mut Vec<Command>) {
    f.held.retain(|w| *w != window);
    if matches!(f.phase, Phase::Holding { .. }) && f.held.is_empty() {
        start_moving(f, now, out);
    }
}

fn start_moving(f: &mut Flight, now: Instant, out: &mut Vec<Command>) {
    if f.clock.is_some() {
        return;
    }
    out.push(Command::StartMoving);
    f.phase = Phase::Playing;
    f.clock = Some((now, f.stage.duration));
    f.snap_landed(out);
}

fn end_watch(f: &Flight, now: Instant) -> EndWatch {
    let (awaiting, landing) = f.end_windows();
    EndWatch {
        generation: f.generation,
        landing,
        awaiting,
        landing_until: now + LANDING_PATIENCE,
        awaiting_until: now + HOLD_CAP,
        focus: watched_focus(f),
        losing_focus: watched_losing(f),
        background: f.stage.background(),
        background_initial: f.background_later.then(|| now + Duration::from_millis(30)),
        behind: f.stage.behind(),
    }
}

/// The focus target and its end, while a focus report is coming and its focused look is not in. A
/// watch armed by a report that never comes would hold the background back for the whole flight.
fn watched_focus(f: &Flight) -> Option<(WindowId, ScreenRect)> {
    let window = f.stage.focus.filter(|w| f.focus_moving() && !f.focus_in && !f.awaiting.contains(w))?;
    let end = f.stage.tile(window)?.end;
    (!is_off_screen(f.stage.display, end)).then_some((window, end))
}

/// The window focus leaves and its end, watched beside the target while its own turn is to come and
/// can be seen, if it ends wholly on the display: SkyLight pictures only that, and the one
/// ScreenCaptureKit capture a flight lets through is the target's. One a pass opened or grew is
/// watched to repaint instead.
fn watched_losing(f: &Flight) -> Option<(WindowId, ScreenRect)> {
    watched_focus(f)?;
    let window = f
        .stage
        .losing_focus
        .filter(|w| f.unfocused != Some(*w) && f.unseen != Some(*w) && !f.awaiting.contains(w))?;
    let end = f.stage.tile(window)?.end;
    wholly_inside(end, f.stage.display).then_some((window, end))
}

fn final_at(started: Instant, duration: Duration) -> Instant {
    (started + duration).checked_sub(FINAL_BEFORE_END).unwrap_or(started).max(started + FINAL_NOT_BEFORE)
}

/// Over every end window watched: those that land, and those that repaint, floating or not, since a
/// window the overlay covers wholly may stop painting.
fn holes(f: &Flight) -> Vec<OverlayRect> {
    let (awaited, landing) = f.end_windows();
    let display = f.stage.display;
    awaited.into_iter().chain(landing).map(|(_, frame)| to_overlay_space(frame, display)).collect()
}

/// Each tile of `after` with the generation since which it has ended where it ends: what `since` says
/// for one ending where it ended in `before`, else `generation`.
fn ended_since(
    since: &[(WindowId, Generation)],
    before: Option<&StageSummary>,
    after: &StageSummary,
    generation: Generation,
) -> Vec<(WindowId, Generation)> {
    after
        .tiles
        .iter()
        .map(|t| {
            let kept = before
                .and_then(|b| b.tile(t.window))
                .filter(|b| near(b.end, t.end, 0.5))
                .and_then(|_| since.iter().find(|(w, _)| *w == t.window).map(|(_, g)| *g));
            (t.window, kept.unwrap_or(generation))
        })
        .collect()
}

fn changed_frames(before: &[(WindowId, ScreenRect)], after: &[(WindowId, ScreenRect)]) -> Vec<(WindowId, ScreenRect)> {
    after
        .iter()
        .filter(|(w, f)| !before.iter().any(|(b, g)| b == w && near(*f, *g, 0.5)))
        .copied()
        .collect()
}

fn extend_unique(into: &mut Vec<WindowId>, more: impl Iterator<Item = WindowId>) {
    for w in more {
        if !into.contains(&w) {
            into.push(w);
        }
    }
}

/// Adds each window with its frame, or moves one already there to its new frame: latest frame per
/// window wins.
pub(in crate::transitions) fn extend_latest(
    into: &mut Vec<(WindowId, ScreenRect)>,
    more: impl Iterator<Item = (WindowId, ScreenRect)>,
) {
    for (window, frame) in more {
        match into.iter_mut().find(|(held, _)| *held == window) {
            Some(held) => held.1 = frame,
            None => into.push((window, frame)),
        }
    }
}

/// Wholly on `display`, and big enough to be a window rather than a sliver.
fn wholly_inside(frame: ScreenRect, display: ScreenRect) -> bool {
    frame.size.width > 100.0 && frame.size.height > 100.0 && wholly_on(frame, display)
}

#[cfg(test)]
pub(in crate::transitions) mod testing;

#[cfg(test)]
mod tests {
    use rini_geometry::space::{Point, Size, SurfaceRect};

    use super::testing::{DISPLAY, PLAYS, pan, r, running, tile, w};
    use super::*;
    use crate::transitions::domain::timing::SETTLE_BEFORE_CAPTURES;

    fn has(out: &[Command], f: impl Fn(&Command) -> bool) -> bool {
        out.iter().any(f)
    }

    fn writes(out: &[Command]) -> Vec<(WindowId, ScreenRect)> {
        out.iter().filter_map(|c| if let Command::Write(f) = c { Some(f.clone()) } else { None }).flatten().collect()
    }

    fn watch(out: &[Command]) -> Option<EndWatch> {
        out.iter().find_map(|c| if let Command::Watch(w) = c { Some(w.clone()) } else { None })
    }

    /// Each tile drawn focused or not, in the order commanded.
    fn focus(out: &[Command]) -> Vec<(WindowId, bool)> {
        out.iter().filter_map(|c| if let Command::Focus { window, focused } = c { Some((*window, *focused)) } else { None }).collect()
    }

    /// The pan's window 1 drawn unfocused, then window 2 focused.
    fn handed_over() -> Vec<(WindowId, bool)> {
        vec![(w(1), false), (w(2), true)]
    }

    /// The swaps, kept looks and focus drawn, in order.
    fn turns(out: &[Command]) -> Vec<Command> {
        out.iter()
            .filter(|c| matches!(c, Command::Swap { .. } | Command::KeepFocusedLook { .. } | Command::Focus { .. }))
            .cloned()
            .collect()
    }

    #[test]
    fn frame_zero_asks_for_the_strip_windows_on_screen_and_the_background_seen_there() {
        let now = Instant::now();
        let mut c = Coordinator::default();
        let out = c.on(Event::Begin(pan()), now);
        assert_eq!(out[0], Command::Hold);
        let Command::AskFrameZero { generation, strip, background, behind } = &out[1] else { panic!("{out:?}") };
        assert_eq!(*generation, Generation(1));
        assert_eq!(strip, &vec![(w(1), r(3.0, 35.0, 860.0, 1079.0))], "the window parked off screen is not asked for");
        assert_eq!(background.len(), 1, "the floating window is seen through the strip's gaps");
        assert_eq!(behind, &vec![w(9)], "and composited behind the strip, which may blur it");
        assert!(has(&out, |c| *c == Command::ReportUp));
        assert!(!has(&out, |c| matches!(c, Command::Show | Command::Write(_))), "nothing shows or moves before frame zero");
    }

    #[test]
    fn a_background_wholly_behind_one_opaque_window_waits_for_the_overlay() {
        let now = Instant::now();
        let mut stage = pan();
        stage.tiles[2].start = r(100.0, 100.0, 400.0, 300.0);
        stage.tiles[2].end = stage.tiles[2].start;
        stage.opaque = vec![w(1)];
        let mut c = Coordinator::default();
        let out = c.on(Event::Begin(stage.clone()), now);
        let Command::AskFrameZero { background, .. } = &out[1] else { panic!() };
        assert!(background.is_empty());
        let out = c.on(Event::FrameZero(c.generation()), now);
        let watch = watch(&out).unwrap();
        assert_eq!(watch.background.len(), 1);
        assert_eq!(watch.background_initial, Some(now + Duration::from_millis(30)));

        // Behind a window not known to be opaque, it is in frame zero.
        stage.opaque.clear();
        let mut c = Coordinator::default();
        let out = c.on(Event::Begin(stage), now);
        let Command::AskFrameZero { background, .. } = &out[1] else { panic!() };
        assert_eq!(background.len(), 1);
    }

    #[test]
    fn the_overlay_waits_for_its_frame_zero_pictures_or_80ms() {
        let now = Instant::now();
        let mut c = Coordinator::default();
        c.on(Event::Begin(pan()), now);
        let out = c.on(Event::Tick { settled: true, landed: true }, now + Duration::from_millis(79));
        assert!(out.is_empty());
        assert!(!c.is_shown());
        let out = c.on(Event::Tick { settled: true, landed: true }, now + FRAME_ZERO_PATIENCE);
        assert_eq!(out.first(), Some(&Command::Show));
        assert!(c.is_shown() && c.is_moving());
    }

    /// Frame zero slower than the overlay's patience: the overlay is up and moving on the cache's
    /// pictures. The late pictures go onto their tiles where they are, the window losing focus's kept as
    /// its focused look, and nothing is shown, written or installed again.
    #[test]
    fn frame_zero_after_the_overlay_went_up_without_it_only_swaps_its_pictures_in() {
        let now = Instant::now();
        let mut c = Coordinator::default();
        c.on(Event::Begin(pan()), now);
        let g = c.generation();
        assert_eq!(c.on(Event::Tick { settled: false, landed: false }, now + FRAME_ZERO_PATIENCE).first(), Some(&Command::Show));
        let late = now + FRAME_ZERO_PATIENCE + Duration::from_millis(10);
        let start = pan().tiles[0].start;
        let out = c.on(Event::Picture { generation: g, window: w(1), frame: start, why: Why::FrameZero }, late);
        assert_eq!(out, vec![Command::Swap { generation: g, window: w(1) }, Command::KeepFocusedLook { generation: g, window: w(1) }]);
        let mut resized = start;
        resized.size.width -= 21.0;
        assert!(c.on(Event::Picture { generation: g, window: w(1), frame: resized, why: Why::FrameZero }, late).is_empty(),
            "not where its tile starts");
        assert!(c.on(Event::Picture { generation: g, window: w(9), frame: pan().tiles[2].start, why: Why::FrameZero }, late)
            == vec![Command::Swap { generation: g, window: w(9) }]);
        assert!(c.on(Event::FrameZero(g), late).is_empty(), "the show is not run again");
        let mut moved = pan();
        moved.tiles[0].end.origin.x -= 400.0;
        c.on(Event::Merge { stage: moved, moved: true }, late);
        assert!(has(&c.on(Event::Picture { generation: g, window: w(1), frame: start, why: Why::FrameZero }, late),
            |c| matches!(c, Command::Swap { .. })), "wherever its tile ends now");
        assert!(c.on(Event::Picture { generation: Generation(g.0 - 1), window: w(1), frame: start, why: Why::FrameZero }, late).is_empty(),
            "another flight's");
    }

    /// Frame zero's picture of the window focus leaves is kept as its focused look only if that window
    /// was the key window as the press went out: a click has made its target key first, and another
    /// application's panel taking the keys leaves the layout's window drawn unfocused.
    #[test]
    fn frame_zero_keeps_the_window_focus_leaves_as_its_focused_look_only_if_it_was_key() {
        assert_eq!(frame_zero_look(Some(w(1)), Some(w(1))), Some(w(1)));
        assert_eq!(frame_zero_look(Some(w(1)), Some(w(2))), None, "the click's target is key");
        assert_eq!(frame_zero_look(Some(w(1)), None), None, "a panel has the keys");
        assert_eq!(frame_zero_look(None, Some(w(1))), None, "focus moves nowhere");

        let now = Instant::now();
        let mut panel = pan();
        panel.key = None;
        let mut c = Coordinator::default();
        c.on(Event::Begin(panel.clone()), now);
        let out = c.on(Event::FrameZero(c.generation()), now);
        assert!(has(&out, |c| *c == Command::Show));
        assert!(!has(&out, |c| matches!(c, Command::KeepFocusedLook { .. })), "{out:?}");

        let mut c = Coordinator::default();
        c.on(Event::Begin(panel), now);
        let g = c.generation();
        c.on(Event::Tick { settled: false, landed: false }, now + FRAME_ZERO_PATIENCE);
        let late = Event::Picture { generation: g, window: w(1), frame: pan().tiles[0].start, why: Why::FrameZero };
        assert_eq!(c.on(late, now + FRAME_ZERO_PATIENCE), vec![Command::Swap { generation: g, window: w(1) }], "swapped in, not kept");
    }

    /// A pass merged while frame zero is out that brings a window on screen where its tile starts: frame
    /// zero pictures it too, and the overlay waits for both asks, never past the patience the begin set.
    #[test]
    fn a_pass_merged_before_the_show_has_frame_zero_picture_what_it_brought_on_screen() {
        let now = Instant::now();
        let third = r(866.0, 35.0, 860.0, 1079.0);
        let mut c = Coordinator::default();
        c.on(Event::Begin(pan()), now);
        let g = c.generation();
        let mut more = pan();
        more.tiles.push(tile(w(3), third, third));
        let out = c.on(Event::Merge { stage: more.clone(), moved: false }, now + Duration::from_millis(5));
        assert_eq!(out, vec![Command::AskFrameZero { generation: g, strip: vec![(w(3), third)], background: Vec::new(), behind: vec![w(9)] }]);
        assert!(c.on(Event::Merge { stage: more.clone(), moved: false }, now + Duration::from_millis(6)).is_empty(), "asked once");
        assert!(c.on(Event::FrameZero(g), now + Duration::from_millis(30)).is_empty(), "one ask still out");
        assert!(!c.is_shown());
        assert_eq!(c.on(Event::FrameZero(g), now + Duration::from_millis(40)).first(), Some(&Command::KeepFocusedLook { generation: g, window: w(1) }));
        assert!(c.is_shown());

        let mut c = Coordinator::default();
        c.on(Event::Begin(pan()), now);
        let mut moved = pan();
        moved.tiles[0].start.origin.x += 3.0;
        let out = c.on(Event::Merge { stage: moved, moved: false }, now);
        assert!(matches!(out.as_slice(), [Command::AskFrameZero { strip, .. }] if strip.len() == 1), "a start no ask expected: {out:?}");
        c.on(Event::FrameZero(c.generation()), now);
        assert_eq!(c.on(Event::Tick { settled: false, landed: false }, now + FRAME_ZERO_PATIENCE).first(), Some(&Command::Show),
            "the patience is the begin's");
    }

    #[test]
    fn frame_zero_for_another_generation_shows_nothing() {
        let now = Instant::now();
        let mut c = Coordinator::default();
        c.on(Event::Begin(pan()), now);
        assert!(c.on(Event::FrameZero(Generation(7)), now).is_empty());
        assert!(!c.is_shown());
    }

    #[test]
    fn no_real_window_is_written_before_the_overlay_is_up() {
        let now = Instant::now();
        let mut c = Coordinator::default();
        c.on(Event::Begin(pan()), now);
        let mut merged = pan();
        merged.final_frames[1].1.origin.x = 10.0;
        let out = c.on(Event::Merge { stage: merged, moved: true }, now + Duration::from_millis(5));
        assert!(writes(&out).is_empty(), "{out:?}");
        let out = c.on(Event::FrameZero(c.generation()), now + Duration::from_millis(30));
        assert_eq!(writes(&out)[1].1.origin.x, 10.0, "the show writes the merged frames");
    }

    #[test]
    fn the_show_writes_the_frames_cuts_the_holes_watches_the_end_and_moves() {
        let now = Instant::now();
        let mut c = Coordinator::default();
        c.on(Event::Begin(pan()), now);
        let out = c.on(Event::FrameZero(Generation(1)), now);
        assert_eq!(writes(&out), pan().final_frames);
        let holes = out.iter().find_map(|c| if let Command::Holes(h) = c { Some(h.clone()) } else { None }).unwrap();
        assert_eq!(holes.len(), 2);
        let watch = watch(&out).unwrap();
        assert_eq!(watch.landing.len(), 2);
        assert!(watch.awaiting.is_empty());
        assert_eq!(watch.landing_until, now + LANDING_PATIENCE);
        assert_eq!(watch.focus, Some((w(2), pan().tiles[1].end)));
        assert_eq!(watch.behind, vec![w(9)]);
        assert!(has(&out, |c| *c == Command::StartMoving));
        assert!(has(&out, |c| *c == Command::KeepFocusedLook { generation: Generation(1), window: w(1) }),
            "the window losing focus is still drawn focused at frame zero");
    }

    #[test]
    fn a_layout_pass_waits_for_its_follow_ups_and_a_flight_with_newcomers_waits_for_them() {
        let now = Instant::now();
        let mut stage = pan();
        stage.kind = Kind::LayoutPass;
        stage.start = Start::Coalesced;
        let mut c = running(stage.clone(), now);
        assert!(!c.is_moving());
        assert!(c.on(Event::Tick { settled: true, landed: true }, now + Duration::from_millis(24)).is_empty());
        assert_eq!(c.on(Event::Tick { settled: true, landed: true }, now + COALESCE_WINDOW), vec![Command::StartMoving]);

        stage.awaiting = vec![w(2)];
        let mut c = running(stage.clone(), now);
        assert!(!c.is_moving());
        let out = c.on(Event::Picture { generation: c.generation(), window: w(2), frame: stage.tiles[1].end, why: Why::Landed }, now);
        assert!(has(&out, |c| *c == Command::StartMoving), "the awaited picture releases the hold");

        let mut c = running(stage, now);
        let out = c.on(Event::Tick { settled: true, landed: true }, now + HOLD_CAP);
        assert!(has(&out, |c| *c == Command::StartMoving), "or the hold runs out");
    }

    #[test]
    fn awaited_windows_are_watched_until_they_repaint_and_the_rest_as_soon_as_they_are_there() {
        let now = Instant::now();
        let mut stage = pan();
        stage.awaiting = vec![w(2)];
        let mut c = Coordinator::default();
        c.on(Event::Begin(stage.clone()), now);
        let shown = watch(&c.on(Event::FrameZero(c.generation()), now)).unwrap();
        assert_eq!(shown.landing, vec![(w(1), stage.tiles[0].end)]);
        assert_eq!(shown.awaiting, vec![(w(2), stage.tiles[1].end)]);
        assert_eq!(shown.awaiting_until, now + HOLD_CAP, "watched as long as it is held for");
        c.on(Event::Picture { generation: c.generation(), window: w(2), frame: stage.tiles[1].end, why: Why::Landed }, now);
        assert!(c.is_moving());

        // A window grown by a pass merged in flight repaints too, and joins as it lands.
        let mut grown = stage.clone();
        grown.awaiting = vec![w(1)];
        let out = c.on(Event::Merge { stage: grown, moved: false }, now + Duration::from_millis(50));
        let merged = watch(&out).unwrap();
        assert!(merged.landing.is_empty());
        assert_eq!(merged.awaiting.len(), 2, "{merged:?}");
        assert!(!has(&out, |c| *c == Command::StartMoving) && c.is_moving());
    }

    #[test]
    fn the_overlay_holds_only_for_awaited_windows_a_watch_can_see() {
        let now = Instant::now();
        let mut stage = pan();
        stage.awaiting = vec![w(2)];
        let off = r(1727.0, 35.0, 860.0, 1079.0);
        stage.tiles[1].end = off;
        stage.final_frames[1].1 = off;
        let mut c = Coordinator::default();
        c.on(Event::Begin(stage), now);
        let out = c.on(Event::FrameZero(c.generation()), now);
        assert!(has(&out, |c| *c == Command::StartMoving), "a window ending off the display never lands");
        assert!(watch(&out).unwrap().awaiting.is_empty());
    }

    #[test]
    fn a_newcomer_merged_while_the_passes_coalesce_is_held_for() {
        let now = Instant::now();
        let mut stage = pan();
        stage.kind = Kind::LayoutPass;
        stage.start = Start::Coalesced;
        let mut c = running(stage.clone(), now);
        let mut opened = stage.clone();
        opened.awaiting = vec![w(2)];
        c.on(Event::Merge { stage: opened, moved: true }, now + Duration::from_millis(10));
        assert!(c.on(Event::Tick { settled: true, landed: true }, now + COALESCE_WINDOW).is_empty());
        assert!(!c.is_moving());
        let out = c.on(Event::Picture { generation: c.generation(), window: w(2), frame: stage.tiles[1].end, why: Why::Landed }, now + COALESCE_WINDOW);
        assert!(has(&out, |c| *c == Command::StartMoving));
    }

    /// A newcomer whose picture comes, or is given up on, while the passes coalesce is not held for
    /// once they have.
    #[test]
    fn a_newcomer_pictured_or_lost_while_the_passes_coalesce_is_not_held_for() {
        let now = Instant::now();
        let mut stage = pan();
        stage.kind = Kind::LayoutPass;
        stage.start = Start::Coalesced;
        let mut opened = stage.clone();
        opened.awaiting = vec![w(2)];
        for lost in [false, true] {
            let mut c = running(stage.clone(), now);
            c.on(Event::Merge { stage: opened.clone(), moved: true }, now + Duration::from_millis(10));
            let (generation, window) = (c.generation(), w(2));
            let event = match lost {
                true => Event::Lost { generation, window },
                false => Event::Picture { generation, window, frame: stage.tiles[1].end, why: Why::Landed },
            };
            assert!(!has(&c.on(event, now + Duration::from_millis(20)), |c| *c == Command::StartMoving), "still coalescing");
            let out = c.on(Event::Tick { settled: true, landed: true }, now + COALESCE_WINDOW);
            assert_eq!(out, vec![Command::StartMoving], "lost: {lost}");
        }
    }

    /// A held window whose picture the pictures service gives up on, out of time, out of tries or
    /// refused by the cache, is held for no longer: the motion starts once nothing else is, with
    /// nothing swapped or snapped for it. Its repainted picture would have handed focus over; lost,
    /// focus hands over as the last capture is asked.
    #[test]
    fn a_held_window_given_up_on_is_held_for_no_longer() {
        let now = Instant::now();
        let mut stage = pan();
        stage.awaiting = vec![w(2)];
        let mut c = running(stage.clone(), now);
        assert!(!c.is_moving());
        let out = c.on(Event::Lost { generation: c.generation(), window: w(2) }, now);
        assert_eq!(out, vec![Command::StartMoving]);
        let out = c.on(Event::Tick { settled: false, landed: false }, now + Duration::from_millis(240));
        assert!(matches!(out.as_slice(), [Command::AskFinal { .. }, Command::Focus { .. }, Command::Focus { .. }]), "{out:?}");
        assert_eq!(focus(&out), handed_over());

        stage.awaiting = vec![w(1), w(2)];
        let mut c = running(stage.clone(), now);
        let g = c.generation();
        assert!(c.on(Event::Lost { generation: g, window: w(2) }, now).is_empty(), "window 1 is still held for");
        let out = c.on(Event::Picture { generation: g, window: w(1), frame: stage.tiles[0].end, why: Why::Landed }, now);
        assert!(has(&out, |c| *c == Command::StartMoving));
    }

    /// A held window a merged pass sends where no watch can see it repaint, off the display, is held
    /// for no longer: the motion starts at once rather than at the cap.
    #[test]
    fn a_held_window_a_merge_sends_off_the_display_is_held_for_no_longer() {
        let now = Instant::now();
        let mut stage = pan();
        stage.awaiting = vec![w(2)];
        let mut c = running(stage.clone(), now);
        assert!(!c.is_moving());
        let off = r(1727.0, 35.0, 860.0, 1079.0);
        stage.tiles[1].end = off;
        stage.final_frames[1].1 = off;
        let out = c.on(Event::Merge { stage, moved: true }, now + Duration::from_millis(10));
        assert!(watch(&out).unwrap().awaiting.is_empty(), "no watch sees it");
        assert!(has(&out, |c| *c == Command::StartMoving), "{out:?}");
        assert!(c.is_moving());
    }

    /// A merge's retarget watches again a window lost under the generation before, so only a loss under
    /// the generation watched now holds it for no longer.
    #[test]
    fn a_window_lost_under_the_generation_before_a_merge_is_still_held_for() {
        let now = Instant::now();
        let mut stage = pan();
        stage.awaiting = vec![w(2)];
        let mut c = running(stage.clone(), now);
        let before = c.generation();
        c.on(Event::Merge { stage, moved: false }, now + Duration::from_millis(10));
        assert!(c.on(Event::Lost { generation: before, window: w(2) }, now + Duration::from_millis(11)).is_empty());
        assert!(!c.is_moving());
        let out = c.on(Event::Lost { generation: c.generation(), window: w(2) }, now + Duration::from_millis(12));
        assert!(has(&out, |c| *c == Command::StartMoving));
    }

    /// A window given up on is no landing: its tile is not snapped, and the lift still waits for the
    /// real windows where they were written, which the engine reads off the window server.
    #[test]
    fn a_window_given_up_on_is_never_taken_for_landed() {
        let now = Instant::now();
        let mut c = running(pan(), now);
        let g = c.generation();
        for window in [w(1), w(2)] {
            assert!(c.on(Event::Lost { generation: g, window }, now + LANDING_PATIENCE).is_empty());
        }
        let end = now + PLAYS;
        assert!(!has(&c.on(Event::Tick { settled: true, landed: false }, end), |c| matches!(c, Command::Lift { .. })));
        let out = c.on(Event::Tick { settled: true, landed: true }, end + Duration::from_millis(16));
        assert!(has(&out, |c| matches!(c, Command::Lift { .. })));
    }

    #[test]
    fn the_focus_target_is_watched_only_while_its_focused_look_is_to_come() {
        let now = Instant::now();
        let end = pan().tiles[1].end;
        let mut c = running(pan(), now);
        c.on(Event::Picture { generation: c.generation(), window: w(1), frame: pan().tiles[0].end, why: Why::FocusChanged }, now);
        c.on(Event::Picture { generation: c.generation(), window: w(2), frame: end, why: Why::FocusChanged }, now);
        let out = c.on(Event::Merge { stage: pan(), moved: false }, now);
        assert_eq!(watch(&out).unwrap().focus, None, "its look is in");

        let focus_of = |stage: StageSummary| {
            let mut c = Coordinator::default();
            c.on(Event::Begin(stage), now);
            watch(&c.on(Event::FrameZero(c.generation()), now)).unwrap().focus
        };
        assert_eq!(focus_of(pan()), Some((w(2), end)));
        let mut staying = pan();
        staying.losing_focus = None;
        assert_eq!(focus_of(staying), None, "no focus report comes to arm it");
        let mut opened = pan();
        opened.awaiting = vec![w(2)];
        assert_eq!(focus_of(opened), None, "its repainted picture is watched already");
        let mut elsewhere = pan();
        elsewhere.tiles[1].end = r(-900.0, 35.0, 860.0, 1079.0);
        assert_eq!(focus_of(elsewhere), None, "nothing can picture it");
        let mut clipped = pan();
        clipped.tiles[1].end = r(-200.0, 35.0, 860.0, 1079.0);
        assert!(focus_of(clipped).is_some(), "a clipped end takes its one capture");
    }

    #[test]
    fn the_window_losing_focus_is_watched_beside_the_target_while_its_own_turn_is_to_come() {
        let now = Instant::now();
        let losing_of = |stage: StageSummary| {
            let mut c = Coordinator::default();
            c.on(Event::Begin(stage), now);
            watch(&c.on(Event::FrameZero(c.generation()), now)).unwrap().losing_focus
        };
        let end = pan().tiles[0].end;
        assert_eq!(losing_of(pan()), Some((w(1), end)));
        let mut opened = pan();
        opened.awaiting = vec![w(2)];
        assert_eq!(losing_of(opened), None, "only beside a watched target");
        let mut clipped = pan();
        clipped.tiles[0].end = r(1300.0, 35.0, 860.0, 1079.0);
        assert_eq!(losing_of(clipped), None, "SkyLight pictures it only whole, and ScreenCaptureKit's capture is the target's");
        let mut parked = pan();
        parked.tiles[0].end = r(1727.0, 35.0, 860.0, 1079.0);
        assert_eq!(losing_of(parked), None);

        let mut c = running(pan(), now);
        c.on(Event::Picture { generation: c.generation(), window: w(1), frame: end, why: Why::FocusChanged }, now);
        let watched = watch(&c.on(Event::Merge { stage: pan(), moved: false }, now)).unwrap();
        assert_eq!((watched.focus.map(|(window, _)| window), watched.losing_focus), (Some(w(2)), None), "its turn is in");
    }

    /// macOS turns the window focus leaves before the one gaining it. The target's change waits for
    /// the old window's, at most 15ms, and each tile is drawn focused or not with its own picture.
    #[test]
    fn the_target_turns_after_the_window_losing_focus_and_each_ring_with_its_own_picture() {
        let now = Instant::now();
        let (old, new) = (pan().tiles[0].end, pan().tiles[1].end);
        let mut c = running(pan(), now);
        let g = c.generation();
        let out = c.on(Event::Picture { generation: g, window: w(2), frame: new, why: Why::FocusChanged }, now);
        assert_eq!(turns(&out), Vec::new(), "waits for the old window: {out:?}");
        let out = c.on(Event::Picture { generation: g, window: w(1), frame: old, why: Why::FocusChanged }, now + Duration::from_millis(5));
        assert_eq!(
            turns(&out),
            vec![
                Command::Swap { generation: g, window: w(1) },
                Command::Focus { window: w(1), focused: false },
                Command::Swap { generation: g, window: w(2) },
                Command::KeepFocusedLook { generation: g, window: w(2) },
                Command::Focus { window: w(2), focused: true },
            ]
        );

        let mut c = running(pan(), now);
        c.on(Event::Picture { generation: g, window: w(2), frame: new, why: Why::FocusChanged }, now);
        let out = c.on(Event::Tick { settled: false, landed: false }, now + Duration::from_millis(14));
        assert_eq!(turns(&out), Vec::new());
        let out = c.on(Event::Tick { settled: false, landed: false }, now + CHANGE_EVERY);
        assert_eq!(
            turns(&out),
            vec![
                Command::Focus { window: w(1), focused: false },
                Command::Swap { generation: g, window: w(2) },
                Command::KeepFocusedLook { generation: g, window: w(2) },
                Command::Focus { window: w(2), focused: true },
            ],
            "15ms at most, the old window turning unfocused just before"
        );
        let out = c.on(Event::Picture { generation: g, window: w(1), frame: old, why: Why::FocusChanged }, now + Duration::from_millis(20));
        assert_eq!(turns(&out), vec![Command::Swap { generation: g, window: w(1) }], "drawn unfocused once");

        let mut c = running(pan(), now);
        let out = c.on(Event::Picture { generation: g, window: w(1), frame: old, why: Why::FocusChanged }, now);
        assert_eq!(focus(&out), vec![(w(1), false)], "the old window first, with its own picture");
        let out = c.on(Event::Picture { generation: g, window: w(2), frame: new, why: Why::FocusChanged }, now);
        assert!(has(&out, |c| *c == Command::Swap { generation: g, window: w(2) }), "nothing left to wait for");
        assert_eq!(focus(&out), vec![(w(2), true)]);

        let mut clipped = pan();
        clipped.tiles[0].end = r(1300.0, 35.0, 860.0, 1079.0);
        let mut c = running(clipped, now);
        let out = c.on(Event::Picture { generation: g, window: w(2), frame: new, why: Why::FocusChanged }, now);
        assert_eq!(focus(&out), handed_over(), "an old window no watch sees turns just before the target");
        assert_eq!(turns(&out)[1], Command::Swap { generation: g, window: w(2) });

        let mut c = running(pan(), now);
        c.on(Event::Picture { generation: g, window: w(2), frame: new, why: Why::FocusChanged }, now + Duration::from_millis(230));
        let out = c.on(Event::Tick { settled: false, landed: false }, now + Duration::from_millis(240));
        assert!(matches!(out[0], Command::AskFinal { .. }), "{out:?}");
        assert!(has(&out, |c| *c == Command::Swap { generation: g, window: w(2) }), "swapped as the last capture is asked");
        assert_eq!(focus(&out), handed_over());
    }

    /// The old window's turn given up on, clipped where it is, out of tries or refused, the target's
    /// held change waits for it no longer, and nothing more does; under a generation before, it was
    /// given up on for another end.
    #[test]
    fn the_targets_change_waits_no_longer_for_an_old_window_whose_turn_will_not_be_seen() {
        let now = Instant::now();
        let new = pan().tiles[1].end;
        let mut c = running(pan(), now);
        let g = c.generation();
        c.on(Event::Picture { generation: g, window: w(2), frame: new, why: Why::FocusChanged }, now);
        let out = c.on(Event::Lost { generation: Generation(g.0 - 1), window: w(1) }, now + Duration::from_millis(2));
        assert_eq!(turns(&out), Vec::new());
        let out = c.on(Event::Lost { generation: g, window: w(1) }, now + Duration::from_millis(3));
        assert_eq!(
            turns(&out),
            vec![
                Command::Focus { window: w(1), focused: false },
                Command::Swap { generation: g, window: w(2) },
                Command::KeepFocusedLook { generation: g, window: w(2) },
                Command::Focus { window: w(2), focused: true },
            ]
        );

        let mut c = running(pan(), now);
        c.on(Event::Lost { generation: g, window: w(1) }, now);
        let out = c.on(Event::Merge { stage: pan(), moved: false }, now + Duration::from_millis(2));
        assert_eq!(watch(&out).unwrap().losing_focus, None, "watched no more");
        let g = c.generation();
        let out = c.on(Event::Picture { generation: g, window: w(2), frame: new, why: Why::FocusChanged }, now + Duration::from_millis(4));
        assert_eq!(focus(&out), handed_over(), "nothing to wait for");

        let mut growing = pan();
        growing.awaiting = vec![w(1)];
        let mut c = Coordinator::default();
        c.on(Event::Begin(growing), now);
        let watched = watch(&c.on(Event::FrameZero(c.generation()), now)).unwrap();
        assert_eq!(watched.losing_focus, None, "watched to repaint instead");
    }

    /// A rapid switch, 1 to 2 and back before 2 turns: window 1, drawn unfocused with its own
    /// picture, is the target again, watched and handed focus with its focused picture. On to 3
    /// before then, it stays drawn unfocused and is not turned again; on to 3 after, it turns again.
    #[test]
    fn focus_moved_back_to_a_window_already_turned_unfocused_turns_it_again_only_with_its_picture() {
        let now = Instant::now();
        let old = pan().tiles[0].end;
        let third = r(3.0, 35.0, 860.0, 1079.0);
        let back = StageSummary { focus: Some(w(1)), losing_focus: None, ..pan() };
        let on = || {
            let mut stage = StageSummary { focus: Some(w(3)), losing_focus: Some(w(1)), ..pan() };
            stage.tiles.push(tile(w(3), r(-1723.0, 35.0, 860.0, 1079.0), third));
            stage.final_frames.push((w(3), third));
            stage
        };
        let turned_and_back = || {
            let mut c = running(pan(), now);
            let g = c.generation();
            let out = c.on(Event::Picture { generation: g, window: w(1), frame: old, why: Why::FocusChanged }, now);
            assert_eq!(focus(&out), vec![(w(1), false)]);
            let out = c.on(Event::Merge { stage: back.clone(), moved: false }, now + Duration::from_millis(10));
            assert_eq!(focus(&out), Vec::new(), "drawn unfocused until its focused picture: {out:?}");
            let watched = watch(&out).unwrap();
            assert_eq!((watched.focus, watched.losing_focus), (Some((w(1), old)), None), "focus moves back to it");
            c
        };

        let mut c = turned_and_back();
        let out = c.on(Event::Merge { stage: on(), moved: false }, now + Duration::from_millis(20));
        assert_eq!(watch(&out).unwrap().losing_focus, None, "already drawn unfocused");
        let g = c.generation();
        let out = c.on(Event::Picture { generation: g, window: w(3), frame: third, why: Why::FocusChanged }, now + Duration::from_millis(30));
        assert_eq!(focus(&out), vec![(w(3), true)], "nothing waits for it, nothing turns it again");

        let mut c = turned_and_back();
        let g = c.generation();
        let out = c.on(Event::Picture { generation: g, window: w(1), frame: old, why: Why::FocusChanged }, now + Duration::from_millis(20));
        assert_eq!(
            turns(&out),
            vec![
                Command::Swap { generation: g, window: w(1) },
                Command::KeepFocusedLook { generation: g, window: w(1) },
                Command::Focus { window: w(1), focused: true },
            ]
        );
        let out = c.on(Event::Merge { stage: on(), moved: false }, now + Duration::from_millis(30));
        assert_eq!(watch(&out).unwrap().losing_focus, Some((w(1), old)), "drawn focused again, it turns again");
        let g = c.generation();
        let out = c.on(Event::Picture { generation: g, window: w(3), frame: third, why: Why::FocusChanged }, now + Duration::from_millis(40));
        assert_eq!(focus(&out), Vec::new(), "waits for it");
        let out = c.on(Event::Tick { settled: false, landed: false }, now + Duration::from_millis(40) + CHANGE_EVERY);
        assert_eq!(focus(&out), vec![(w(1), false), (w(3), true)]);
    }

    /// 1 to 2, 2 parked and handed focus; on to 3, 2 turning unfocused with its own picture; back to 2,
    /// which flies its focused look and so is drawn focused again; on to 3 before 2 is handed focus:
    /// 2 is the window losing focus again, watched and turned unfocused before 3 is handed focus.
    #[test]
    fn a_window_turned_unfocused_and_back_with_its_focused_look_turns_unfocused_again_as_focus_leaves() {
        let now = Instant::now();
        let ms = Duration::from_millis;
        let (old, mid) = (pan().tiles[0].end, pan().tiles[1].end);
        let third = r(866.0, 35.0, 860.0, 1079.0);
        let to = |focus: u32, losing_focus: Option<u32>| {
            let mut stage = StageSummary {
                focus: Some(w(focus)),
                losing_focus: losing_focus.map(w),
                focused_look: (focus == 2).then_some(w(2)),
                ..pan()
            };
            stage.tiles[0].end = r(1729.0, 35.0, 860.0, 1079.0);
            stage.final_frames[0].1 = stage.tiles[0].end;
            stage.tiles.push(tile(w(3), r(1729.0, 35.0, 860.0, 1079.0), third));
            stage.final_frames.push((w(3), third));
            stage
        };
        let mut c = running(StageSummary { focused_look: Some(w(2)), ..pan() }, now);
        let g = c.generation();
        c.on(Event::Picture { generation: g, window: w(1), frame: old, why: Why::FocusChanged }, now);
        let out = c.on(Event::Picture { generation: g, window: w(2), frame: mid, why: Why::FocusChanged }, now + ms(5));
        assert_eq!(focus(&out), vec![(w(2), true)]);
        c.on(Event::Merge { stage: to(3, Some(2)), moved: false }, now + ms(10));
        let out = c.on(Event::Picture { generation: c.generation(), window: w(2), frame: mid, why: Why::FocusChanged }, now + ms(15));
        assert_eq!(focus(&out), vec![(w(2), false)]);
        c.on(Event::Merge { stage: to(2, None), moved: false }, now + ms(20));
        let out = c.on(Event::Merge { stage: to(3, Some(2)), moved: false }, now + ms(25));
        assert_eq!(watch(&out).unwrap().losing_focus, Some((w(2), mid)), "drawn focused again, it turns again");
        let g = c.generation();
        let out = c.on(Event::Picture { generation: g, window: w(3), frame: third, why: Why::FocusChanged }, now + ms(30));
        assert_eq!(focus(&out), Vec::new(), "waits for it");
        let out = c.on(Event::Tick { settled: false, landed: false }, now + ms(30) + CHANGE_EVERY);
        assert_eq!(focus(&out), vec![(w(2), false), (w(3), true)]);
    }

    /// The old window given up on just as a press keeps focus where it was: its report under the
    /// generation before is dropped, and the one the merge's retarget sends again under the new
    /// generation swaps the target's held change at once.
    #[test]
    fn the_old_window_given_up_on_across_a_merge_keeping_focus_frees_the_held_change_at_once() {
        let now = Instant::now();
        let ms = Duration::from_millis;
        let new = pan().tiles[1].end;
        let mut c = running(pan(), now);
        let g = c.generation();
        c.on(Event::Picture { generation: g, window: w(2), frame: new, why: Why::FocusChanged }, now);
        let out = c.on(Event::Merge { stage: pan(), moved: false }, now + ms(2));
        assert_eq!(watch(&out).unwrap().losing_focus.map(|(window, _)| window), Some(w(1)));
        let out = c.on(Event::Lost { generation: g, window: w(1) }, now + ms(3));
        assert_eq!(turns(&out), Vec::new(), "of the generation before");
        let out = c.on(Event::Lost { generation: c.generation(), window: w(1) }, now + ms(4));
        assert_eq!(
            turns(&out),
            vec![
                Command::Focus { window: w(1), focused: false },
                Command::Swap { generation: g, window: w(2) },
                Command::KeepFocusedLook { generation: g, window: w(2) },
                Command::Focus { window: w(2), focused: true },
            ]
        );
    }

    /// The target's change held for the old window is of the target focus left: a press moving focus
    /// on drops it, and the new target is not handed focus with it.
    #[test]
    fn a_held_change_is_dropped_when_a_press_moves_focus_on() {
        let now = Instant::now();
        let mut three = pan();
        three.tiles.push(tile(w(3), r(-1723.0, 35.0, 860.0, 1079.0), pan().tiles[1].end));
        let mut c = running(three.clone(), now);
        let g = c.generation();
        c.on(Event::Picture { generation: g, window: w(2), frame: pan().tiles[1].end, why: Why::FocusChanged }, now);
        c.on(Event::Merge { stage: StageSummary { focus: Some(w(3)), ..three }, moved: false }, now + Duration::from_millis(5));
        let out = c.on(Event::Tick { settled: false, landed: false }, now + Duration::from_millis(5) + CHANGE_EVERY);
        assert_eq!(turns(&out), Vec::new(), "{out:?}");
        let out = c.on(Event::Picture { generation: c.generation(), window: w(1), frame: pan().tiles[0].end, why: Why::FocusChanged }, now + Duration::from_millis(30));
        assert_eq!(turns(&out), vec![Command::Swap { generation: c.generation(), window: w(1) }, Command::Focus { window: w(1), focused: false }]);
    }

    /// A held change no longer fitting its tile, a press resizing the target meanwhile, turns nothing:
    /// its ring waits for a picture of it that fits.
    #[test]
    fn a_held_change_that_no_longer_fits_hands_nothing_over() {
        let now = Instant::now();
        let mut c = running(pan(), now);
        let g = c.generation();
        c.on(Event::Picture { generation: g, window: w(2), frame: pan().tiles[1].end, why: Why::FocusChanged }, now);
        let mut wider = pan();
        wider.tiles[1].end.size.width = 1200.0;
        wider.final_frames[1].1.size.width = 1200.0;
        let end = wider.tiles[1].end;
        c.on(Event::Merge { stage: wider, moved: true }, now + Duration::from_millis(5));
        let out = c.on(Event::Tick { settled: false, landed: false }, now + CHANGE_EVERY);
        assert_eq!(turns(&out), Vec::new(), "{out:?}");
        let g = c.generation();
        c.on(Event::Picture { generation: g, window: w(1), frame: pan().tiles[0].end, why: Why::FocusChanged }, now + Duration::from_millis(20));
        let out = c.on(Event::Picture { generation: g, window: w(2), frame: end, why: Why::FocusChanged }, now + Duration::from_millis(30));
        assert_eq!(
            turns(&out),
            vec![
                Command::Swap { generation: g, window: w(2) },
                Command::KeepFocusedLook { generation: g, window: w(2) },
                Command::Focus { window: w(2), focused: true },
            ]
        );
    }

    #[test]
    fn a_focus_target_parked_off_the_display_is_the_one_that_flies_its_focused_look() {
        let mut stage = pan();
        assert_eq!(stage.parked_focus(), Some(w(2)));
        stage.focus = Some(w(1));
        assert_eq!(stage.parked_focus(), None, "frame zero pictures it");
        stage.focus = None;
        assert_eq!(stage.parked_focus(), None);
    }

    #[test]
    fn a_merged_press_starts_a_new_generation_and_rewrites_only_what_changed() {
        let now = Instant::now();
        let mut c = running(pan(), now);
        let before = c.generation();
        let mut next = pan();
        next.final_frames[1].1.origin.x = 20.0;
        next.tiles[1].end.origin.x = 20.0;
        next.focus = Some(w(1));
        let out = c.on(Event::Merge { stage: next, moved: true }, now + Duration::from_millis(100));
        assert_eq!(c.generation(), Generation(before.0 + 1));
        assert_eq!(writes(&out), vec![(w(2), r(20.0, 35.0, 860.0, 1079.0))]);
        let watch = watch(&out).unwrap();
        assert_eq!(watch.generation, c.generation());
        assert_eq!(watch.focus.map(|(window, _)| window), Some(w(1)));
        assert_eq!(c.remaining(now + Duration::from_millis(100)), Some(PLAYS), "a merge that moves restarts the clock");
    }

    /// The service decides a delivery under the generation it was asked for, and a pass merged while
    /// it is on its way bumps the generation: the picture still shows the window where its tile ends,
    /// and the service will not deliver it again.
    #[test]
    fn a_picture_taken_before_a_merge_that_kept_its_end_is_still_drawn_and_hands_focus_over() {
        let now = Instant::now();
        let mut c = running(pan(), now);
        let old = c.generation();
        let end = pan().tiles[1].end;
        c.on(Event::Merge { stage: pan(), moved: false }, now);
        assert_ne!(c.generation(), old);
        c.on(Event::Picture { generation: old, window: w(1), frame: pan().tiles[0].end, why: Why::FocusChanged }, now);
        let out = c.on(Event::Picture { generation: old, window: w(2), frame: end, why: Why::FocusChanged }, now);
        assert!(has(&out, |c| *c == Command::Swap { generation: old, window: w(2) }), "{out:?}");
        assert!(has(&out, |c| *c == Command::KeepFocusedLook { generation: old, window: w(2) }), "named as it was delivered");
        assert_eq!(focus(&out), vec![(w(2), true)]);
    }

    #[test]
    fn a_picture_taken_for_an_end_its_tile_no_longer_has_or_of_another_frame_is_never_drawn() {
        let now = Instant::now();
        let mut c = running(pan(), now);
        let old = c.generation();
        let end = pan().tiles[1].end;
        let mut moved = pan();
        moved.tiles[1].end.origin.x += 400.0;
        moved.final_frames[1].1.origin.x += 400.0;
        c.on(Event::Merge { stage: moved.clone(), moved: true }, now);
        assert!(c.on(Event::Picture { generation: old, window: w(2), frame: moved.tiles[1].end, why: Why::Landed }, now).is_empty(),
            "taken for the end before, whatever frame it carries");
        assert!(has(&c.on(Event::Picture { generation: old, window: w(1), frame: pan().tiles[0].end, why: Why::Landed }, now),
            |c| matches!(c, Command::Swap { .. })), "the window whose end the merge kept");
        let g = c.generation();
        assert!(c.on(Event::Picture { generation: g, window: w(2), frame: end, why: Why::Landed }, now).is_empty());
        let mut far = moved.tiles[1].end;
        far.size.width -= 21.0;
        assert!(c.on(Event::Picture { generation: g, window: w(2), frame: far, why: Why::Landed }, now).is_empty(),
            "a picture of the window at another size would be drawn squashed or zoomed");
        let out = c.on(Event::Picture { generation: g, window: w(2), frame: moved.tiles[1].end, why: Why::Landed }, now);
        assert!(has(&out, |c| *c == Command::Swap { generation: g, window: w(2) }), "{out:?}");
    }

    #[test]
    fn a_tile_a_point_off_its_window_snaps_onto_it_once_per_generation() {
        let now = Instant::now();
        let mut c = running(pan(), now);
        let g = c.generation();
        let mut real = pan().tiles[1].end;
        real.size.width += 1.0;
        let picture = |c: &mut Coordinator, g, frame| c.on(Event::Picture { generation: g, window: w(2), frame, why: Why::Landed }, now);
        assert!(has(&picture(&mut c, g, real), |c| matches!(c, Command::Snap { .. })));
        assert!(!has(&picture(&mut c, g, real), |c| matches!(c, Command::Snap { .. })), "once");
        let merged = c.on(Event::Merge { stage: pan(), moved: false }, now);
        assert!(has(&merged, |c| *c == Command::Snap { window: w(2), frame: to_overlay_space(real, DISPLAY) }),
            "again with a merge, which may have put the tile back on its model, and no new print will come");
        let g = c.generation();
        assert!(!has(&picture(&mut c, g, real), |c| matches!(c, Command::Snap { .. })), "once a generation");
        let mut moved = pan();
        moved.tiles[1].end.origin.x += 400.0;
        let merged = c.on(Event::Merge { stage: moved, moved: true }, now);
        assert!(!has(&merged, |c| matches!(c, Command::Snap { .. })), "a window ending elsewhere lands again first");
    }

    /// On a display away from the origin, the holes and the snaps are the overlay's points: the end
    /// frames and the window's frame less the display's origin, as on the main display.
    #[test]
    fn holes_and_snaps_are_drawn_in_overlay_space_on_a_display_away_from_the_origin() {
        let now = Instant::now();
        let display = r(-1728.0, -300.0, 1728.0, 1117.0);
        let on_it = |frame: ScreenRect| frame.translate(display.origin.to_vector());
        let mut stage = pan();
        stage.display = display;
        for tile in &mut stage.tiles {
            (tile.start, tile.end) = (on_it(tile.start), on_it(tile.end));
        }
        for (_, frame) in &mut stage.final_frames {
            *frame = on_it(*frame);
        }
        let mut c = Coordinator::default();
        c.on(Event::Begin(stage), now);
        let out = c.on(Event::FrameZero(c.generation()), now);
        let holes = out.iter().find_map(|c| if let Command::Holes(h) = c { Some(h.clone()) } else { None }).unwrap();
        let at_home = |frame: ScreenRect| to_overlay_space(frame, DISPLAY);
        assert_eq!(holes, pan().final_frames.iter().map(|(_, end)| at_home(*end)).collect::<Vec<_>>());

        let mut real = pan().tiles[1].end;
        real.size.width += 1.0;
        let landed = Event::Picture { generation: c.generation(), window: w(2), frame: on_it(real), why: Why::Landed };
        let out = c.on(landed, now);
        assert!(has(&out, |c| *c == Command::Snap { window: w(2), frame: at_home(real) }), "{out:?}");
    }

    #[test]
    fn a_tile_snaps_only_once_the_motion_has_started() {
        let now = Instant::now();
        let mut stage = pan();
        stage.kind = Kind::LayoutPass;
        stage.start = Start::Coalesced;
        let mut c = running(stage, now);
        let mut real = pan().tiles[1].end;
        real.size.width += 1.0;
        let out = c.on(Event::Picture { generation: c.generation(), window: w(2), frame: real, why: Why::Landed }, now);
        assert!(has(&out, |c| matches!(c, Command::Swap { .. })));
        assert!(!has(&out, |c| matches!(c, Command::Snap { .. })), "frame zero still stands; the start would move it back");
        let out = c.on(Event::Tick { settled: true, landed: true }, now + COALESCE_WINDOW);
        assert_eq!(out, vec![Command::StartMoving, Command::Snap { window: w(2), frame: to_overlay_space(real, DISPLAY) }]);
    }

    #[test]
    fn a_window_flying_from_where_it_opened_is_watched_until_it_repaints_and_never_held_for() {
        let now = Instant::now();
        let mut stage = pan();
        stage.repainting = vec![w(2)];
        let mut c = Coordinator::default();
        c.on(Event::Begin(stage.clone()), now);
        let out = c.on(Event::FrameZero(c.generation()), now);
        assert!(has(&out, |c| *c == Command::StartMoving), "it flies at once");
        let watch = watch(&out).unwrap();
        assert_eq!(watch.awaiting, vec![(w(2), stage.tiles[1].end)], "watched until it has repainted");
        assert_eq!(watch.landing, vec![(w(1), stage.tiles[0].end)]);
        assert_eq!(watch.focus, None, "its repainted picture is the handover");
        let out = c.on(Event::Picture { generation: c.generation(), window: w(2), frame: stage.tiles[1].end, why: Why::Landed }, now);
        assert_eq!(focus(&out), handed_over());

        // Merged in while the overlay holds for another, it does not add to the hold.
        let mut grown = pan();
        grown.awaiting = vec![w(1)];
        let mut c = running(grown.clone(), now);
        let mut opened = grown;
        opened.awaiting.clear();
        opened.repainting = vec![w(2)];
        c.on(Event::Merge { stage: opened.clone(), moved: false }, now);
        let out = c.on(Event::Picture { generation: c.generation(), window: w(1), frame: opened.tiles[0].end, why: Why::Landed }, now);
        assert!(has(&out, |c| *c == Command::StartMoving));
    }

    #[test]
    fn a_floating_window_awaited_is_watched_held_for_and_left_a_hole() {
        let now = Instant::now();
        let dialog = r(500.0, 300.0, 700.0, 500.0);
        let mut stage = pan();
        stage.tiles.push(TileSummary { window: w(7), start: dialog, end: dialog, floating: true, companion: false });
        stage.final_frames.push((w(7), dialog));
        stage.awaiting = vec![w(7)];
        let mut c = Coordinator::default();
        c.on(Event::Begin(stage), now);
        let out = c.on(Event::FrameZero(c.generation()), now);
        assert!(!has(&out, |c| *c == Command::StartMoving), "held for its first picture");
        assert_eq!(watch(&out).unwrap().awaiting, vec![(w(7), dialog)]);
        let holes = out.iter().find_map(|c| if let Command::Holes(h) = c { Some(h.clone()) } else { None }).unwrap();
        assert!(holes.contains(&to_overlay_space(dialog, DISPLAY)), "a window the overlay covers wholly may not paint");
        let g = c.generation();
        let out = c.on(Event::Picture { generation: g, window: w(7), frame: dialog, why: Why::Landed }, now);
        assert!(has(&out, |c| *c == Command::Swap { generation: g, window: w(7) }));
        assert!(has(&out, |c| *c == Command::StartMoving));
    }

    #[test]
    fn what_is_left_of_the_clock_and_when_a_tick_reads_the_landing() {
        let now = Instant::now();
        let mut stage = pan();
        stage.start = Start::Coalesced;
        stage.kind = Kind::LayoutPass;
        let mut c = running(stage, now);
        assert_eq!(c.remaining(now), None, "not moving yet");
        assert!(!c.wants_landing(now + Duration::from_secs(5)));
        c.on(Event::Tick { settled: false, landed: false }, now + COALESCE_WINDOW);
        let started = now + COALESCE_WINDOW;
        assert_eq!(c.remaining(started + Duration::from_millis(100)), Some(PLAYS - Duration::from_millis(100)));
        assert!(!c.wants_landing(started + PLAYS - Duration::from_millis(1)));
        assert!(c.wants_landing(started + PLAYS));
        assert_eq!(c.remaining(started + 2 * PLAYS), Some(Duration::ZERO));
        c.on(Event::Extend(Duration::from_millis(200)), started + PLAYS);
        assert!(!c.wants_landing(started + PLAYS), "an accent stretched it");
        assert_eq!(Coordinator::default().remaining(now), None);
    }

    #[test]
    fn the_window_focus_leaves_is_drawn_focused_and_the_target_only_once_it_has_the_look() {
        let (from, to) = (Some(w(1)), Some(w(2)));
        assert_eq!(losing_focus(to, from), from);
        assert_eq!(losing_focus(to, to), None, "drawn focused already");
        assert_eq!(losing_focus(None, from), None, "a pass naming no focus moves none");
        assert!(drawn_focused(w(1), to, from), "frame zero pictures it focused");
        assert!(!drawn_focused(w(2), to, from), "until focus hands over");
        assert!(drawn_focused(w(2), to, None), "focus is not moving: it has it");
        assert!(!drawn_focused(w(3), to, from));
        assert!(!drawn_focused(w(3), None, None));
    }

    #[test]
    fn a_strip_movement_is_a_move_when_a_column_crosses_it_and_a_switch_when_it_travels_between_rows() {
        let frame = SurfaceRect::new(Point::new(4.0, 32.0), Size::new(859.0, 1081.0));
        let window = |from| SurfaceWindow {
            window: w(1),
            server_id: rini_core::ids::WindowServerId::new(1),
            frame,
            pinned: false,
            floating: false,
            from,
        };
        let rest = SurfacePoint::origin();
        assert_eq!(Kind::of_surface(&[window(None)], SurfacePoint::new(863.0, 0.0), rest), Kind::Pan);
        assert_eq!(Kind::of_surface(&[window(None)], SurfacePoint::new(0.0, -1085.0), rest), Kind::Switch);
        assert_eq!(Kind::of_surface(&[window(Some(frame))], SurfacePoint::new(863.0, 0.0), rest), Kind::Move);
    }

    #[test]
    fn a_delivery_is_a_clients_own_under_the_generation_its_tag_carries() {
        let tag = Tag { client: Client(1), generation: 5 };
        assert_eq!(Generation::for_client(tag, Client(1)), Some(Generation(5)));
        assert_eq!(Generation::for_client(tag, Client(2)), None, "another client's delivery is not drawn");
    }

    #[test]
    fn a_later_pass_overwrites_a_windows_destination() {
        let mut frames = vec![(w(1), r(0., 0., 100., 100.))];
        extend_latest(&mut frames, [(w(1), r(500., 0., 100., 100.))].into_iter());
        assert_eq!(frames, [(w(1), r(500., 0., 100., 100.))]);
        extend_latest(&mut frames, [(w(1), r(500., 0., 100., 100.))].into_iter());
        assert_eq!(frames, [(w(1), r(500., 0., 100., 100.))], "a repeat changes nothing");
    }

    #[test]
    fn a_window_not_in_the_flight_is_appended() {
        let mut frames = vec![(w(1), r(0., 0., 100., 100.))];
        extend_latest(&mut frames, [(w(2), r(200., 0., 100., 100.))].into_iter());
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[1], (w(2), r(200., 0., 100., 100.)));
    }

    #[test]
    fn nothing_incoming_changes_nothing() {
        let mut frames = vec![(w(1), r(0., 0., 100., 100.))];
        extend_latest(&mut frames, std::iter::empty());
        assert_eq!(frames, [(w(1), r(0., 0., 100., 100.))]);
    }

    #[test]
    fn the_focus_targets_changed_picture_is_kept_as_its_focused_look_and_hands_focus_over() {
        let now = Instant::now();
        let mut c = running(pan(), now);
        let g = c.generation();
        let end = pan().tiles[1].end;
        let out = c.on(Event::Picture { generation: g, window: w(1), frame: pan().tiles[0].end, why: Why::FocusChanged }, now);
        assert!(!has(&out, |c| matches!(c, Command::KeepFocusedLook { .. })), "only the focus target's");
        assert_eq!(focus(&out), vec![(w(1), false)], "the window left turns unfocused with it");
        let landed = c.on(Event::Picture { generation: g, window: w(2), frame: end, why: Why::Landed }, now);
        assert_eq!(focus(&landed), Vec::new(), "a landing is not the handover");
        let out = c.on(Event::Picture { generation: g, window: w(2), frame: end, why: Why::FocusChanged }, now);
        assert!(has(&out, |c| *c == Command::KeepFocusedLook { generation: g, window: w(2) }));
        assert_eq!(focus(&out), vec![(w(2), true)]);

        // A window opened or grown as the focus target hands over when it has repainted.
        let mut opened = pan();
        opened.awaiting = vec![w(2)];
        let mut c = running(opened, now);
        let out = c.on(Event::Picture { generation: c.generation(), window: w(2), frame: end, why: Why::Landed }, now);
        assert_eq!(focus(&out), handed_over());
        assert!(!has(&out, |c| matches!(c, Command::KeepFocusedLook { .. })));
    }

    #[test]
    fn the_last_capture_of_the_focus_target_hands_focus_over_if_its_change_never_came() {
        let now = Instant::now();
        let mut c = running(pan(), now);
        let g = c.generation();
        let out = c.on(Event::Picture { generation: g, window: w(2), frame: pan().tiles[1].end, why: Why::Final }, now);
        assert_eq!(focus(&out), handed_over());
        assert!(!has(&out, |c| matches!(c, Command::KeepFocusedLook { .. })), "not known to be its focused look");
        let out = c.on(Event::Picture { generation: g, window: w(2), frame: pan().tiles[1].end, why: Why::FocusChanged }, now);
        assert_eq!(focus(&out), Vec::new(), "handed over once");
    }

    /// The target looked focused before anything watched it change, or its look shows focus no other
    /// way: nothing hands focus over, so the last capture's ask does, while the flight still moves.
    #[test]
    fn a_focus_target_whose_change_never_shows_hands_focus_over_as_the_last_capture_is_asked() {
        let now = Instant::now();
        let mut c = running(pan(), now);
        let g = c.generation();
        let out = c.on(Event::Picture { generation: g, window: w(2), frame: pan().tiles[1].end, why: Why::Landed }, now);
        assert_eq!(focus(&out), Vec::new());
        let out = c.on(Event::Tick { settled: false, landed: false }, now + Duration::from_millis(240));
        assert!(matches!(out.as_slice(), [Command::AskFinal { .. }, Command::Focus { .. }, Command::Focus { .. }]), "{out:?}");
        assert_eq!(focus(&out), handed_over(), "neither turn showed: both flip, the old window first");
        let out = c.on(Event::Picture { generation: g, window: w(2), frame: pan().tiles[1].end, why: Why::FocusChanged }, now);
        assert!(has(&out, |c| *c == Command::KeepFocusedLook { generation: g, window: w(2) }), "a change after it is still its look");
        assert_eq!(focus(&out), Vec::new(), "handed over once");

        let mut staying = pan();
        staying.losing_focus = None;
        let mut c = running(staying, now);
        let out = c.on(Event::Tick { settled: false, landed: false }, now + Duration::from_millis(240));
        assert_eq!(focus(&out), Vec::new(), "focus moves nowhere");
    }

    /// A parked target flying its focused look: its landing is often taken before it repaints
    /// focused, so the look stays over it until focus hands over, and the landing is swapped in then
    /// if nothing newer came.
    #[test]
    fn a_parked_focus_target_keeps_its_focused_look_over_its_landing_until_focus_hands_over() {
        let now = Instant::now();
        let mut stage = pan();
        stage.focused_look = Some(w(2));
        let end = stage.tiles[1].end;
        let mut c = running(stage.clone(), now);
        let g = c.generation();
        let out = c.on(Event::Picture { generation: g, window: w(2), frame: end, why: Why::Landed }, now);
        assert!(!has(&out, |c| matches!(c, Command::Swap { .. })), "{out:?}");
        c.on(Event::Picture { generation: g, window: w(1), frame: stage.tiles[0].end, why: Why::FocusChanged }, now);
        let out = c.on(Event::Picture { generation: g, window: w(2), frame: end, why: Why::FocusChanged }, now);
        assert_eq!(out[0], Command::Swap { generation: g, window: w(2) });
        assert!(has(&out, |c| *c == Command::KeepFocusedLook { generation: g, window: w(2) }));
        assert_eq!(focus(&out), vec![(w(2), true)]);
        assert_eq!(out.iter().filter(|c| matches!(c, Command::Swap { .. })).count(), 1, "the landing held back is not swapped as well");

        let mut c = running(stage.clone(), now);
        let g = c.generation();
        c.on(Event::Picture { generation: g, window: w(2), frame: end, why: Why::Landed }, now);
        let out = c.on(Event::Tick { settled: false, landed: false }, now + Duration::from_millis(240));
        assert!(has(&out, |c| *c == Command::Swap { generation: g, window: w(2) }), "swapped in as focus hands over: {out:?}");
        assert_eq!(focus(&out), handed_over());
        let out = c.on(Event::Picture { generation: g, window: w(2), frame: end, why: Why::Landed }, now);
        assert!(has(&out, |c| matches!(c, Command::Swap { .. })), "once handed over, a landing is swapped");

        let mut c = running(stage, now);
        let g = c.generation();
        c.on(Event::Picture { generation: g, window: w(2), frame: end, why: Why::Landed }, now);
        let mut moved = pan();
        moved.focused_look = Some(w(2));
        moved.tiles[1].end.origin.x += 400.0;
        c.on(Event::Merge { stage: moved, moved: true }, now);
        let out = c.on(Event::Tick { settled: false, landed: false }, now + Duration::from_millis(240));
        assert!(!has(&out, |c| matches!(c, Command::Swap { .. })), "a landing taken for the end before: {out:?}");
    }

    #[test]
    fn the_last_capture_is_asked_110ms_before_the_end_and_never_in_the_first_40() {
        let now = Instant::now();
        let mut c = running(pan(), now);
        assert!(c.on(Event::Tick { settled: false, landed: false }, now + Duration::from_millis(239)).is_empty());
        let out = c.on(Event::Tick { settled: false, landed: false }, now + Duration::from_millis(240));
        assert!(matches!(out.as_slice(), [Command::AskFinal { windows, behind, .. }, ..] if windows.len() == 2 && *behind == vec![w(9)]));
        assert!(c.on(Event::Tick { settled: false, landed: false }, now + Duration::from_millis(250)).is_empty(), "once");
        assert_eq!(final_at(now, Duration::from_millis(100)), now + FINAL_NOT_BEFORE);
    }

    #[test]
    fn the_lift_waits_for_the_tiles_and_the_windows_or_the_grace() {
        let now = Instant::now();
        let mut c = running(pan(), now);
        let end = now + PLAYS;
        let out = c.on(Event::Tick { settled: true, landed: false }, end);
        assert!(!has(&out, |c| matches!(c, Command::Lift { .. })));
        let out = c.on(Event::Tick { settled: true, landed: true }, end + Duration::from_millis(16));
        assert!(has(&out, |c| matches!(c, Command::Lift { .. })));
        assert!(!c.is_flying());

        let mut c = running(pan(), now);
        let out = c.on(Event::Tick { settled: false, landed: false }, end + LIFT_GRACE);
        assert!(has(&out, |c| matches!(c, Command::Lift { .. })), "overdue");
    }

    /// Passes repeating the flight's destinations after its clock ran out, while a window has not
    /// landed, do not hold off the lift past its grace.
    #[test]
    fn passes_moving_nothing_after_the_end_leave_the_lift_its_grace() {
        let now = Instant::now();
        let mut c = running(pan(), now);
        let end = now + PLAYS;
        let mut at = end;
        while at < end + LIFT_GRACE {
            assert!(!has(&c.on(Event::Tick { settled: true, landed: false }, at), |c| matches!(c, Command::Lift { .. })));
            c.on(Event::Merge { stage: pan(), moved: false }, at + Duration::from_millis(1));
            at += Duration::from_millis(100);
        }
        let out = c.on(Event::Tick { settled: true, landed: false }, end + LIFT_GRACE);
        assert!(has(&out, |c| matches!(c, Command::Lift { .. })), "{out:?}");
    }

    #[test]
    fn the_service_is_held_at_the_begin_settling_at_the_lift_and_released_at_the_quiet() {
        let now = Instant::now();
        let mut c = Coordinator::default();
        assert_eq!(c.on(Event::Begin(pan()), now).first(), Some(&Command::Hold));
        c.on(Event::FrameZero(c.generation()), now);
        let out = c.on(Event::Tick { settled: true, landed: true }, now + PLAYS);
        let generation = c.generation();
        assert!(out.ends_with(&[Command::StopWatches, Command::Lift { generation }, Command::Settle]), "{out:?}");
        assert!(!has(&out, |c| *c == Command::Release), "the rest waits for the quiet");
        let out = c.on(Event::Quiet(generation), now + PLAYS + SETTLE_BEFORE_CAPTURES);
        assert!(matches!(out.as_slice(), [Command::Release, Command::Warm(_), Command::ReportSettled]), "{out:?}");
    }

    /// A press inside the quiet holds the service again before anything is composed; the quiet of
    /// the lift before it releases nothing, and only its own quiet does.
    #[test]
    fn a_press_inside_the_quiet_holds_again_and_only_its_own_quiet_releases() {
        let now = Instant::now();
        let lift = |out: &[Command]| {
            out.iter().find_map(|c| if let Command::Lift { generation } = c { Some(*generation) } else { None })
        };
        let mut c = running(pan(), now);
        let first = lift(&c.on(Event::Tick { settled: true, landed: true }, now + PLAYS)).unwrap();
        let pressed = now + PLAYS + Duration::from_millis(100);
        assert_eq!(c.on(Event::Begin(pan()), pressed).first(), Some(&Command::Hold));
        assert!(c.on(Event::Quiet(first), now + PLAYS + SETTLE_BEFORE_CAPTURES).is_empty(), "the lift before's");
        c.on(Event::FrameZero(c.generation()), pressed);
        let second = lift(&c.on(Event::Tick { settled: true, landed: true }, pressed + PLAYS)).unwrap();
        assert_ne!(first, second);
        let out = c.on(Event::Quiet(second), pressed + PLAYS + SETTLE_BEFORE_CAPTURES);
        assert!(matches!(out.as_slice(), [Command::Release, Command::Warm(_), Command::ReportSettled]), "{out:?}");
    }

    #[test]
    fn the_quiet_warms_every_window_the_flight_drew_where_it_last_ended() {
        let now = Instant::now();
        let mut c = running(pan(), now);
        let mut more = pan();
        let parked = r(1727.0, 35.0, 860.0, 1079.0);
        more.tiles.push(tile(w(5), parked, parked));
        more.tiles[0].end.size.width = 600.0;
        c.on(Event::Merge { stage: more, moved: false }, now);
        let out = c.on(Event::Tick { settled: true, landed: true }, now + PLAYS);
        let Some(Command::Lift { generation }) = out.iter().find(|c| matches!(c, Command::Lift { .. })).cloned() else { panic!() };
        assert!(c.on(Event::Quiet(Generation(0)), now).is_empty(), "a stale quiet");
        let out = c.on(Event::Quiet(generation), now + PLAYS + SETTLE_BEFORE_CAPTURES);
        let Some(Command::Warm(windows)) = out.iter().find(|c| matches!(c, Command::Warm(_))) else { panic!() };
        assert!(windows.contains(&(w(5), parked)), "a window only the merged pass drew is warmed too");
        assert!(windows.contains(&(w(1), r(866.0, 35.0, 600.0, 1079.0))), "at the size the merge left it");
        assert_eq!(windows.len(), 4);
    }

    #[test]
    fn a_press_inside_the_quiet_continues_the_flight_reported_and_carries_its_warms() {
        let now = Instant::now();
        let mut c = running(pan(), now);
        c.on(Event::Tick { settled: true, landed: true }, now + PLAYS);
        let mut next = pan();
        next.tiles.retain(|t| t.window != w(9));
        let out = c.on(Event::Begin(next), now + PLAYS + Duration::from_millis(100));
        assert!(!has(&out, |c| *c == Command::ReportUp));
        c.on(Event::FrameZero(c.generation()), now + PLAYS + Duration::from_millis(100));
        let out = c.on(Event::Tick { settled: true, landed: true }, now + 2 * PLAYS + Duration::from_millis(100));
        let Some(Command::Lift { generation }) = out.iter().find(|c| matches!(c, Command::Lift { .. })).cloned() else { panic!() };
        let out = c.on(Event::Quiet(generation), now + Duration::from_secs(2));
        let Some(Command::Warm(windows)) = out.iter().find(|c| matches!(c, Command::Warm(_))) else { panic!() };
        assert!(windows.iter().any(|(window, _)| *window == w(9)), "the first flight's windows are still owed");
    }

    #[test]
    fn a_bounce_writes_nothing_watches_nothing_and_an_accent_stretches_its_clock() {
        let now = Instant::now();
        let mut stage = pan();
        stage.kind = Kind::Bounce;
        for t in &mut stage.tiles {
            t.end = t.start;
        }
        let mut c = Coordinator::default();
        c.on(Event::Begin(stage), now);
        let out = c.on(Event::FrameZero(c.generation()), now);
        assert!(writes(&out).is_empty() && watch(&out).is_none(), "{out:?}");
        c.on(Event::Extend(Duration::from_millis(500)), now + Duration::from_millis(300));
        let out = c.on(Event::Tick { settled: true, landed: true }, now + PLAYS);
        assert!(!has(&out, |c| matches!(c, Command::Lift { .. })), "the clock now runs to 800ms");
        let out = c.on(Event::Tick { settled: true, landed: true }, now + Duration::from_millis(800));
        assert!(has(&out, |c| matches!(c, Command::Lift { .. })));
    }

    #[test]
    fn an_abandoned_transition_places_its_windows_pictures_the_looks_it_owed_and_releases_the_hold() {
        let now = Instant::now();
        let mut c = Coordinator::default();
        c.on(Event::Begin(pan()), now);
        let turned = vec![(w(2), pan().tiles[1].end), (w(1), pan().tiles[0].start)];
        c.on(Event::LooksTurned(turned.clone()), now);
        let out = c.on(Event::Abandon, now);
        assert_eq!(writes(&out), pan().final_frames);
        assert!(has(&out, |c| *c == Command::Release) && has(&out, |c| *c == Command::ReportSettled));
        assert!(has(&out, |c| *c == Command::Refresh(turned.clone())), "nothing flew to picture them");
        assert!(!c.is_flying());
        assert!(c.on(Event::Abandon, now).is_empty());
    }

    /// A focus change alters a window's look and never its size, and a warm skips a picture that
    /// fits: with nothing flying, both ends are asked for again at once.
    #[test]
    fn a_look_turned_at_rest_is_pictured_afresh_at_once() {
        let now = Instant::now();
        let mut c = Coordinator::default();
        let turned = vec![(w(2), r(3.0, 35.0, 860.0, 1079.0)), (w(1), r(866.0, 35.0, 860.0, 1079.0))];
        assert_eq!(c.on(Event::LooksTurned(turned.clone()), now), vec![Command::Refresh(turned)]);
        assert!(c.on(Event::LooksTurned(Vec::new()), now).is_empty());
        assert!(!c.is_flying());
    }

    /// In flight, the watches picture the ends they can and the quiet pictures the rest: here the
    /// target's change came in, while the window losing focus had only its frame-zero picture, taken
    /// before the turn, and the window parked at both ends had none.
    #[test]
    fn a_look_turned_in_flight_is_pictured_at_the_quiet_unless_the_flight_pictured_it_since() {
        let now = Instant::now();
        let stage = pan();
        let (a_start, a_end, b_end) = (stage.tiles[0].start, stage.tiles[0].end, stage.tiles[1].end);
        let parked = r(1727.0, 35.0, 860.0, 1079.0);
        let mut c = running(stage, now);
        let turned = vec![(w(2), b_end), (w(1), a_end), (w(5), parked)];
        assert!(c.on(Event::LooksTurned(turned), now).is_empty(), "nothing is asked behind the overlay");
        let g = c.generation();
        c.on(Event::Picture { generation: g, window: w(1), frame: a_start, why: Why::FrameZero }, now);
        let changed = Duration::from_millis(60);
        c.on(Event::Picture { generation: g, window: w(2), frame: b_end, why: Why::FocusChanged }, now + changed);
        let out = c.on(Event::Tick { settled: true, landed: true }, now + PLAYS);
        let Some(Command::Lift { generation }) = out.iter().find(|c| matches!(c, Command::Lift { .. })).cloned() else { panic!("{out:?}") };
        let out = c.on(Event::Quiet(generation), now + PLAYS + SETTLE_BEFORE_CAPTURES);
        assert_eq!(
            out,
            vec![
                Command::Release,
                Command::Warm(vec![(w(1), a_end), (w(2), b_end), (w(9), r(224.0, 95.0, 600.0, 400.0))]),
                Command::Refresh(vec![(w(1), a_end), (w(5), parked)]),
                Command::ReportSettled,
            ]
        );
    }

    /// A turn reported after the lift waits for the quiet, and a press inside the quiet carries it to
    /// its own quiet, unless that flight pictures it.
    #[test]
    fn a_look_turned_while_settling_waits_for_the_quiet_a_press_inside_it_carries() {
        let now = Instant::now();
        let mut c = running(pan(), now);
        let lifted = c.on(Event::Tick { settled: true, landed: true }, now + PLAYS);
        let Some(Command::Lift { generation: first }) = lifted.iter().find(|c| matches!(c, Command::Lift { .. })).cloned() else { panic!() };
        let parked = r(1727.0, 35.0, 860.0, 1079.0);
        let after = now + PLAYS + Duration::from_millis(50);
        assert!(c.on(Event::LooksTurned(vec![(w(5), parked), (w(2), pan().tiles[1].end)]), after).is_empty());
        let pressed = after + Duration::from_millis(50);
        c.on(Event::Begin(pan()), pressed);
        assert!(c.on(Event::Quiet(first), now + PLAYS + SETTLE_BEFORE_CAPTURES).is_empty());
        let g = c.generation();
        c.on(Event::FrameZero(g), pressed);
        c.on(Event::Picture { generation: g, window: w(2), frame: pan().tiles[1].end, why: Why::Landed }, pressed);
        let out = c.on(Event::Tick { settled: true, landed: true }, pressed + PLAYS);
        let Some(Command::Lift { generation: second }) = out.iter().find(|c| matches!(c, Command::Lift { .. })).cloned() else { panic!() };
        let out = c.on(Event::Quiet(second), pressed + PLAYS + SETTLE_BEFORE_CAPTURES);
        assert!(has(&out, |c| *c == Command::Refresh(vec![(w(5), parked)])), "{out:?}");
    }

    #[test]
    fn generations_only_grow() {
        let now = Instant::now();
        let mut c = Coordinator::default();
        let mut last = c.generation();
        for i in 0..20u64 {
            let at = now + Duration::from_millis(i * 50);
            if c.is_flying() {
                c.on(Event::Merge { stage: pan(), moved: i % 2 == 0 }, at);
            } else {
                c.on(Event::Begin(pan()), at);
                c.on(Event::FrameZero(c.generation()), at);
            }
            c.on(Event::Tick { settled: true, landed: true }, at + PLAYS);
            assert!(c.generation() >= last);
            last = c.generation();
        }
    }
}
