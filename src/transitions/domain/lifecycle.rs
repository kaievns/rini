//! The lifecycle every transition runs through, whatever moves in it: frame-zero pictures, the
//! overlay up, the real frames written, end-state pictures swapped in as the windows land, a press
//! merged in flight, the lift, and the quiet after it. Pure: every event carries its instant, and the
//! shell carries out the commands. See `docs/pictures-motion-transitions.md`.
//!
//! What a kind stages and how its plan merges stay with the kind and the motion; the coordinator sees
//! a [`StageSummary`] and decides only when.
use std::collections::HashSet;
use std::time::{Duration, Instant};

use objc2_core_foundation::CGRect;
use rini_core::ids::WindowId;

use super::timing::{COALESCE_WINDOW, HOLD_CAP, LIFT_GRACE};

/// How long the overlay waits for its frame-zero pictures before showing what the cache holds.
pub const FRAME_ZERO_PATIENCE: Duration = Duration::from_millis(80);
/// How long the end windows are watched to land after their frames are written.
pub const LANDING_PATIENCE: Duration = Duration::from_millis(150);
/// The last end-state capture starts this long before the clock ends, so a slow capture still lands.
pub const FINAL_BEFORE_END: Duration = Duration::from_millis(110);
/// ... and never sooner than this after the motion starts.
pub const FINAL_NOT_BEFORE: Duration = Duration::from_millis(40);
/// A picture is drawn on its tile only if the frame it was taken at is this close to the tile's end.
pub const FIT_SLACK: f64 = 20.0;
/// A tile is moved onto its window's real frame when it ends between these two distances off it.
pub const SNAP_FROM: f64 = 0.25;

/// One press, or one burst merged in flight. Every picture asked for carries it, so one taken for an
/// earlier end state is never drawn.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Generation(pub u64);

/// What kind of change a transition is. The lifecycle is the same for all; each kind decides the few
/// things below.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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

/// One drawn window, in screen points.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TileSummary {
    pub window: WindowId,
    pub start: CGRect,
    pub end: CGRect,
    pub floating: bool,
    /// A border tracing another window: drawn, never pictured on its own here.
    pub companion: bool,
}

impl TileSummary {
    fn moves(&self) -> bool {
        !same_frame(self.start, self.end, 0.5)
    }
}

/// What a kind staged, as far as the lifecycle needs to know.
#[derive(Clone, Debug, PartialEq)]
pub struct StageSummary {
    pub kind: Kind,
    pub tiles: Vec<TileSummary>,
    /// Where the real windows go: every moving window, drawn or not.
    pub final_frames: Vec<(WindowId, CGRect)>,
    pub focus: Option<WindowId>,
    /// The window focus leaves. Its frame-zero picture still shows it focused.
    pub losing_focus: Option<WindowId>,
    /// Windows held for, overlay up, until their first picture lands: new and growing ones.
    pub awaiting: Vec<WindowId>,
    pub start: Start,
    /// The motion's length. A merge that moves anything restarts the clock with its own.
    pub duration: Duration,
    /// The display the overlay covers, in screen points.
    pub display: CGRect,
    /// Windows nothing shows through: measured opaque, no material.
    pub opaque: Vec<WindowId>,
}

impl StageSummary {
    /// Strip windows wholly on screen at their start: what frame zero must picture fresh.
    fn frame_zero_strip(&self) -> Vec<(WindowId, CGRect)> {
        self.tiles
            .iter()
            .filter(|t| !t.floating && !t.companion && wholly_inside(t.start, self.display))
            .map(|t| (t.window, t.start))
            .collect()
    }

    /// Floating windows that stay put, wholly on screen: what shows behind the strip.
    fn background(&self) -> Vec<(WindowId, CGRect)> {
        self.tiles
            .iter()
            .filter(|t| t.floating && !t.companion && !t.moves() && wholly_inside(t.start, self.display))
            .map(|t| (t.window, t.start))
            .collect()
    }

    /// The background windows seen at frame zero, and whether any was left for later: one wholly
    /// behind a single opaque strip window at its start cannot be seen until the strip moves.
    fn background_at_frame_zero(&self) -> (Vec<(WindowId, CGRect)>, bool) {
        let strip = self.frame_zero_strip();
        let (seen, hidden): (Vec<_>, Vec<_>) = self.background().into_iter().partition(|(_, frame)| {
            !strip.iter().any(|(w, s)| contains(*s, *frame) && self.opaque.contains(w))
        });
        (seen, !hidden.is_empty())
    }

    /// Strip windows that end wholly on screen: whose landing is watched and whose tiles get holes.
    fn end_windows(&self) -> Vec<(WindowId, CGRect)> {
        self.final_frames
            .iter()
            .filter(|(w, f)| {
                wholly_inside(*f, self.display)
                    && self.tiles.iter().any(|t| t.window == *w && !t.floating && !t.companion)
            })
            .copied()
            .collect()
    }

    fn tile(&self, window: WindowId) -> Option<&TileSummary> {
        self.tiles.iter().find(|t| t.window == window)
    }

    fn animated(&self) -> impl Iterator<Item = WindowId> + '_ {
        self.tiles.iter().map(|t| t.window)
    }
}

/// Why the pictures service delivered a picture.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Why {
    /// The window landed where it ends.
    Landed,
    /// The focus target's picture changed after it was reported focused: its focused look.
    FocusChanged,
    /// A floating window behind the strip, refreshed.
    Background,
    /// The last capture before the lift.
    Final,
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
    /// One end-state picture, taken with the window at `frame`.
    Picture { generation: Generation, window: WindowId, frame: CGRect, why: Why },
    /// An accent playing past the clock (the edge bounce) needs it to run this much longer from now.
    Extend(Duration),
    /// The frame clock. `settled`: every tile is drawn where it ends. `landed`: the real windows are where
    /// they were written.
    Tick { settled: bool, landed: bool },
    /// The quiet after a lift ran out. Carries the generation that lifted, so a stale one is ignored.
    Quiet(Generation),
    /// The shell could not stage or show the transition: no overlay, no display.
    Abandon,
}

/// What the shell carries out.
#[derive(Clone, Debug, PartialEq)]
pub enum Command {
    /// Hold off-screen capture work while the transition runs.
    Hold,
    Release,
    /// Fresh pictures of the windows on screen at their start, and the background seen there.
    AskFrameZero { generation: Generation, strip: Vec<(WindowId, CGRect)>, background: Vec<(WindowId, CGRect)> },
    /// Overlay up, at frame zero.
    Show,
    Write(Vec<(WindowId, CGRect)>),
    /// A point of the overlay left clear over each window that ends on screen, so the window server
    /// counts it visible and the app keeps painting.
    Holes(Vec<CGRect>),
    /// Watch the end state: each window in `landing` until it lands, the focus target until its
    /// look changes, and the background, from `background_from` on, kept fresh.
    Watch(EndWatch),
    StopWatches,
    StartMoving,
    /// One more capture of every end window, before the lift.
    AskFinal { generation: Generation, windows: Vec<(WindowId, CGRect)> },
    /// Draw the delivered picture on its window's tile.
    Swap { generation: Generation, window: WindowId },
    /// Move the tile onto its window's real frame.
    Snap { window: WindowId, frame: CGRect },
    /// The picture just swapped in shows the window focused: keep it as its focused look.
    KeepFocusedLook { generation: Generation, window: WindowId },
    /// Overlay down. The quiet that follows ends with `Event::Quiet(generation)`.
    Lift { generation: Generation },
    Warm(Vec<WindowId>),
    /// The overlay went up for a flight not already reported.
    ReportUp,
    ReportSettled,
}

#[derive(Clone, Debug, PartialEq)]
pub struct EndWatch {
    pub generation: Generation,
    pub landing: Vec<(WindowId, CGRect)>,
    pub landing_until: Instant,
    pub focus: Option<WindowId>,
    pub background: Vec<(WindowId, CGRect)>,
    /// The background waits for the focus target's picture, except one capture this soon when it was
    /// left out of frame zero.
    pub background_initial: Option<Instant>,
}

#[derive(Clone, Debug, PartialEq)]
enum Phase {
    /// Waiting for the frame-zero pictures, overlay not up.
    Preparing { deadline: Instant },
    /// Overlay up, waiting for the follow-up passes.
    Coalescing { until: Instant },
    /// Overlay up, waiting for the awaited windows' first pictures.
    Holding { deadline: Instant },
    Playing,
    /// The clock is done; waiting for the tiles and the windows to be where they end.
    Landing,
}

#[derive(Clone, Debug, PartialEq)]
struct Flight {
    generation: Generation,
    stage: StageSummary,
    phase: Phase,
    /// When the motion started, and how long it runs from then.
    clock: Option<(Instant, Duration)>,
    /// The background was left out of frame zero.
    background_later: bool,
    focus_in: bool,
    final_asked: bool,
    snapped: HashSet<WindowId>,
    /// Every window this flight drew, across its merged passes: what the quiet warms.
    animated: Vec<WindowId>,
}

#[derive(Clone, Debug, Default, PartialEq)]
enum State {
    #[default]
    Idle,
    Flying(Box<Flight>),
    Settling { generation: Generation, owed: Vec<WindowId> },
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

    /// How far the motion is, 0 to 1; `None` before it starts.
    pub fn progress(&self, now: Instant) -> Option<f64> {
        let State::Flying(f) = &self.state else { return None };
        let (started, duration) = f.clock?;
        Some(if duration.is_zero() { 1.0 } else { (now - started).as_secs_f64() / duration.as_secs_f64() }.min(1.0))
    }

    pub fn on(&mut self, event: Event, now: Instant) -> Vec<Command> {
        let mut out = Vec::new();
        match event {
            Event::Begin(stage) => self.begin(stage, now, &mut out),
            Event::Merge { stage, moved } => self.merge(stage, moved, now, &mut out),
            Event::FrameZero(generation) => {
                if let State::Flying(f) = &mut self.state
                    && f.generation == generation
                    && matches!(f.phase, Phase::Preparing { .. })
                {
                    if let Some(window) = f.stage.losing_focus {
                        out.push(Command::KeepFocusedLook { generation, window });
                    }
                    show(f, now, &mut out);
                }
            }
            Event::Picture { generation, window, frame, why } => self.picture(generation, window, frame, why, now, &mut out),
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
                    out.push(Command::ReportSettled);
                }
            }
            Event::Quiet(generation) => {
                if let State::Settling { generation: g, owed } = &mut self.state
                    && *g == generation
                {
                    out.push(Command::Warm(std::mem::take(owed)));
                    out.push(Command::ReportSettled);
                    self.state = State::Idle;
                }
            }
        }
        out
    }

    fn begin(&mut self, stage: StageSummary, now: Instant, out: &mut Vec<Command>) {
        // A press inside the quiet carries the work owed to the next quiet, and the flight already
        // reported stays the one reported.
        let (owed, reported) = match std::mem::take(&mut self.state) {
            State::Settling { owed, .. } => (owed, true),
            State::Idle => (Vec::new(), false),
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
        out.push(Command::Hold);
        out.push(Command::AskFrameZero { generation, strip: stage.frame_zero_strip(), background });
        let mut animated = owed;
        extend_unique(&mut animated, stage.animated());
        self.state = State::Flying(Box::new(Flight {
            generation,
            stage,
            phase: Phase::Preparing { deadline: now + FRAME_ZERO_PATIENCE },
            clock: None,
            background_later,
            focus_in: false,
            final_asked: false,
            snapped: HashSet::new(),
            animated,
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
        extend_unique(&mut f.animated, stage.animated());
        let changed = changed_frames(&f.stage.final_frames, &stage.final_frames);
        let focus_moved = f.stage.focus != stage.focus;
        // Until the motion starts, a newcomer is held for too; once moving, it joins as it lands.
        let mut awaiting = std::mem::take(&mut f.stage.awaiting);
        if f.clock.is_none() {
            extend_unique(&mut awaiting, stage.awaiting.iter().copied());
        }
        f.stage = StageSummary { awaiting, ..stage };
        if matches!(f.phase, Phase::Preparing { .. }) {
            // Nothing is shown or written yet: the show takes the merged stage as it is.
            return;
        }
        self.generation.0 += 1;
        f.generation = self.generation;
        f.snapped.clear();
        f.final_asked = false;
        if focus_moved {
            f.focus_in = false;
        }
        if f.stage.kind.writes() && !changed.is_empty() {
            out.push(Command::Write(changed));
        }
        if moved && let Some((started, duration)) = &mut f.clock {
            *started = now;
            *duration = f.stage.duration;
            f.phase = Phase::Playing;
        }
        out.push(Command::Holes(holes(&f.stage)));
        if f.stage.kind.watches_end() {
            out.push(Command::Watch(end_watch(f, now)));
        }
    }

    fn picture(&mut self, generation: Generation, window: WindowId, frame: CGRect, why: Why, now: Instant, out: &mut Vec<Command>) {
        let State::Flying(f) = &mut self.state else { return };
        if generation != f.generation {
            return;
        }
        let Some(tile) = f.stage.tile(window) else { return };
        if !same_frame(frame, tile.end, FIT_SLACK) {
            return;
        }
        out.push(Command::Swap { generation, window });
        let off = largest_offset(frame, tile.end);
        if (SNAP_FROM..=FIT_SLACK).contains(&off) && f.snapped.insert(window) {
            out.push(Command::Snap { window, frame });
        }
        if why == Why::FocusChanged && f.stage.focus == Some(window) {
            f.focus_in = true;
            out.push(Command::KeepFocusedLook { generation, window });
        }
        if let Phase::Holding { .. } = f.phase {
            f.stage.awaiting.retain(|w| *w != window);
            if f.stage.awaiting.is_empty() {
                start_moving(f, now, out);
            }
        }
    }

    fn tick(&mut self, settled: bool, landed: bool, now: Instant, out: &mut Vec<Command>) {
        let State::Flying(f) = &mut self.state else { return };
        match f.phase.clone() {
            Phase::Preparing { deadline } => {
                if now >= deadline {
                    show(f, now, out);
                }
            }
            Phase::Coalescing { until } => {
                if now >= until {
                    start_moving(f, now, out);
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
                    out.push(Command::AskFinal { generation: f.generation, windows: f.stage.end_windows() });
                }
                if now < end {
                    return;
                }
                f.phase = Phase::Landing;
                let overdue = now >= end + LIFT_GRACE;
                if (settled && landed) || overdue {
                    let generation = f.generation;
                    let owed = std::mem::take(&mut f.animated);
                    out.push(Command::StopWatches);
                    out.push(Command::Lift { generation });
                    out.push(Command::Release);
                    self.state = State::Settling { generation, owed };
                }
            }
        }
    }
}

/// The overlay goes up: the real windows are written behind it, and the end state is watched.
fn show(f: &mut Flight, now: Instant, out: &mut Vec<Command>) {
    out.push(Command::Show);
    if f.stage.kind.writes() && !f.stage.final_frames.is_empty() {
        out.push(Command::Write(f.stage.final_frames.clone()));
    }
    out.push(Command::Holes(holes(&f.stage)));
    if f.stage.kind.watches_end() {
        out.push(Command::Watch(end_watch(f, now)));
    }
    if !f.stage.awaiting.is_empty() {
        f.phase = Phase::Holding { deadline: now + HOLD_CAP };
    } else if f.stage.start == Start::Coalesced {
        f.phase = Phase::Coalescing { until: now + COALESCE_WINDOW };
    } else {
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
}

fn end_watch(f: &Flight, now: Instant) -> EndWatch {
    EndWatch {
        generation: f.generation,
        landing: f.stage.end_windows(),
        landing_until: now + LANDING_PATIENCE,
        focus: f.stage.focus,
        background: f.stage.background(),
        background_initial: f.background_later.then(|| now + Duration::from_millis(30)),
    }
}

fn final_at(started: Instant, duration: Duration) -> Instant {
    (started + duration).checked_sub(FINAL_BEFORE_END).unwrap_or(started).max(started + FINAL_NOT_BEFORE)
}

fn holes(stage: &StageSummary) -> Vec<CGRect> {
    stage.end_windows().into_iter().map(|(_, f)| f).collect()
}

fn changed_frames(before: &[(WindowId, CGRect)], after: &[(WindowId, CGRect)]) -> Vec<(WindowId, CGRect)> {
    after
        .iter()
        .filter(|(w, f)| !before.iter().any(|(b, g)| b == w && same_frame(*f, *g, 0.5)))
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

fn same_frame(a: CGRect, b: CGRect, slack: f64) -> bool {
    largest_offset(a, b) <= slack
}

fn largest_offset(a: CGRect, b: CGRect) -> f64 {
    [
        a.origin.x - b.origin.x,
        a.origin.y - b.origin.y,
        a.size.width - b.size.width,
        a.size.height - b.size.height,
    ]
    .into_iter()
    .map(f64::abs)
    .fold(0.0, f64::max)
}

/// Wholly on `display` within a point, and big enough to be a window rather than a sliver.
fn wholly_inside(frame: CGRect, display: CGRect) -> bool {
    frame.size.width > 100.0
        && frame.size.height > 100.0
        && contains(display, frame)
}

fn contains(outer: CGRect, inner: CGRect) -> bool {
    inner.origin.x >= outer.origin.x - 1.0
        && inner.origin.y >= outer.origin.y - 1.0
        && inner.origin.x + inner.size.width <= outer.origin.x + outer.size.width + 1.0
        && inner.origin.y + inner.size.height <= outer.origin.y + outer.size.height + 1.0
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;

    use objc2_core_foundation::{CGPoint, CGSize};

    use super::*;
    use crate::transitions::domain::timing::SETTLE_BEFORE_CAPTURES;

    fn w(idx: u32) -> WindowId {
        WindowId { pid: 1, idx: NonZeroU32::new(idx).unwrap() }
    }

    fn r(x: f64, y: f64, w: f64, h: f64) -> CGRect {
        CGRect::new(CGPoint::new(x, y), CGSize::new(w, h))
    }

    const DISPLAY: CGRect = CGRect { origin: CGPoint { x: 0.0, y: 0.0 }, size: CGSize { width: 1728.0, height: 1117.0 } };
    const PLAYS: Duration = Duration::from_millis(350);

    fn tile(window: WindowId, start: CGRect, end: CGRect) -> TileSummary {
        TileSummary { window, start, end, floating: false, companion: false }
    }

    /// The strip scrolling one column left: window 1 leaves on the right, window 2 comes in from the
    /// left, focused. Window 9 floats behind the strip and stays.
    fn pan() -> StageSummary {
        let a_start = r(3.0, 35.0, 860.0, 1079.0);
        let a_end = r(866.0, 35.0, 860.0, 1079.0);
        let b_start = r(-860.0, 35.0, 860.0, 1079.0);
        let b_end = r(3.0, 35.0, 860.0, 1079.0);
        let floating = r(224.0, 95.0, 600.0, 400.0);
        StageSummary {
            kind: Kind::Pan,
            tiles: vec![
                tile(w(1), a_start, a_end),
                tile(w(2), b_start, b_end),
                TileSummary { window: w(9), start: floating, end: floating, floating: true, companion: false },
            ],
            final_frames: vec![(w(1), a_end), (w(2), b_end)],
            focus: Some(w(2)),
            losing_focus: Some(w(1)),
            awaiting: Vec::new(),
            start: Start::Immediate,
            duration: PLAYS,
            display: DISPLAY,
            opaque: Vec::new(),
        }
    }

    fn running(stage: StageSummary, at: Instant) -> Coordinator {
        let mut c = Coordinator::default();
        c.on(Event::Begin(stage), at);
        let g = c.generation();
        c.on(Event::FrameZero(g), at);
        c
    }

    fn has(out: &[Command], f: impl Fn(&Command) -> bool) -> bool {
        out.iter().any(f)
    }

    fn writes(out: &[Command]) -> Vec<(WindowId, CGRect)> {
        out.iter().filter_map(|c| if let Command::Write(f) = c { Some(f.clone()) } else { None }).flatten().collect()
    }

    fn watch(out: &[Command]) -> Option<EndWatch> {
        out.iter().find_map(|c| if let Command::Watch(w) = c { Some(w.clone()) } else { None })
    }

    #[test]
    fn frame_zero_asks_for_the_strip_windows_on_screen_and_the_background_seen_there() {
        let now = Instant::now();
        let mut c = Coordinator::default();
        let out = c.on(Event::Begin(pan()), now);
        assert_eq!(out[0], Command::Hold);
        let Command::AskFrameZero { generation, strip, background } = &out[1] else { panic!("{out:?}") };
        assert_eq!(*generation, Generation(1));
        assert_eq!(strip, &vec![(w(1), r(3.0, 35.0, 860.0, 1079.0))], "the window parked off screen is not asked for");
        assert_eq!(background.len(), 1, "the floating window is seen through the strip's gaps");
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
        assert_eq!(watch.landing_until, now + LANDING_PATIENCE);
        assert_eq!(watch.focus, Some(w(2)));
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
        assert_eq!(watch.focus, Some(w(1)));
        assert_eq!(c.progress(now + Duration::from_millis(100)), Some(0.0), "a merge that moves restarts the clock");
    }

    #[test]
    fn a_picture_from_an_older_generation_or_another_frame_is_never_drawn() {
        let now = Instant::now();
        let mut c = running(pan(), now);
        let old = c.generation();
        let end = pan().tiles[1].end;
        c.on(Event::Merge { stage: pan(), moved: false }, now);
        assert!(c.on(Event::Picture { generation: old, window: w(2), frame: end, why: Why::Landed }, now).is_empty());
        let g = c.generation();
        let mut far = end;
        far.size.width -= 21.0;
        assert!(c.on(Event::Picture { generation: g, window: w(2), frame: far, why: Why::Landed }, now).is_empty(),
            "a picture of the window at another size would be drawn squashed or zoomed");
        let out = c.on(Event::Picture { generation: g, window: w(2), frame: end, why: Why::Landed }, now);
        assert_eq!(out, vec![Command::Swap { generation: g, window: w(2) }]);
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
        c.on(Event::Merge { stage: pan(), moved: false }, now);
        let g = c.generation();
        assert!(has(&picture(&mut c, g, real), |c| matches!(c, Command::Snap { .. })), "again after a merge");
        let mut rounding = pan().tiles[1].end;
        rounding.origin.x += 0.2;
        c.on(Event::Merge { stage: pan(), moved: false }, now);
        let g = c.generation();
        assert!(!has(&picture(&mut c, g, rounding), |c| matches!(c, Command::Snap { .. })), "not for rounding");
    }

    #[test]
    fn the_focus_targets_changed_picture_is_kept_as_its_focused_look() {
        let now = Instant::now();
        let mut c = running(pan(), now);
        let g = c.generation();
        let end = pan().tiles[1].end;
        let out = c.on(Event::Picture { generation: g, window: w(2), frame: end, why: Why::FocusChanged }, now);
        assert!(has(&out, |c| *c == Command::KeepFocusedLook { generation: g, window: w(2) }));
        let out = c.on(Event::Picture { generation: g, window: w(1), frame: pan().tiles[0].end, why: Why::FocusChanged }, now);
        assert!(!has(&out, |c| matches!(c, Command::KeepFocusedLook { .. })), "only the focus target's");
    }

    #[test]
    fn the_last_capture_is_asked_110ms_before_the_end_and_never_in_the_first_40() {
        let now = Instant::now();
        let mut c = running(pan(), now);
        assert!(c.on(Event::Tick { settled: false, landed: false }, now + Duration::from_millis(239)).is_empty());
        let out = c.on(Event::Tick { settled: false, landed: false }, now + Duration::from_millis(240));
        assert!(matches!(out.as_slice(), [Command::AskFinal { windows, .. }] if windows.len() == 2));
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
        assert!(has(&out, |c| matches!(c, Command::Lift { .. })) && has(&out, |c| *c == Command::Release));
        assert!(!c.is_flying());

        let mut c = running(pan(), now);
        let out = c.on(Event::Tick { settled: false, landed: false }, end + LIFT_GRACE);
        assert!(has(&out, |c| matches!(c, Command::Lift { .. })), "overdue");
    }

    #[test]
    fn the_quiet_warms_every_window_the_flight_drew_and_reports_it_settled() {
        let now = Instant::now();
        let mut c = running(pan(), now);
        let mut more = pan();
        more.tiles.push(tile(w(5), r(1727.0, 35.0, 860.0, 1079.0), r(1727.0, 35.0, 860.0, 1079.0)));
        c.on(Event::Merge { stage: more, moved: false }, now);
        let out = c.on(Event::Tick { settled: true, landed: true }, now + PLAYS);
        let Some(Command::Lift { generation }) = out.iter().find(|c| matches!(c, Command::Lift { .. })).cloned() else { panic!() };
        assert!(c.on(Event::Quiet(Generation(0)), now).is_empty(), "a stale quiet");
        let out = c.on(Event::Quiet(generation), now + PLAYS + SETTLE_BEFORE_CAPTURES);
        let Command::Warm(windows) = &out[0] else { panic!() };
        assert!(windows.contains(&w(5)), "a window only the merged pass drew is warmed too");
        assert_eq!(out[1], Command::ReportSettled);
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
        let Command::Warm(windows) = &out[0] else { panic!() };
        assert!(windows.contains(&w(9)), "the first flight's windows are still owed");
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
    fn an_abandoned_transition_places_its_windows_and_releases_the_hold() {
        let now = Instant::now();
        let mut c = Coordinator::default();
        c.on(Event::Begin(pan()), now);
        let out = c.on(Event::Abandon, now);
        assert_eq!(writes(&out), pan().final_frames);
        assert!(has(&out, |c| *c == Command::Release) && has(&out, |c| *c == Command::ReportSettled));
        assert!(!c.is_flying());
        assert!(c.on(Event::Abandon, now).is_empty());
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
