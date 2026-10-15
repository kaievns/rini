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
    OverlayRect, OverlayVector, Point, ScreenRect, ScreenSize, Size, SurfacePoint,
};
use rini_geometry::{from_overlay_space, to_overlay_space};
use rini_runloop::channel;
use rini_runloop::run_loop::RepeatingTimer;
use tracing::{debug, warn};

use crate::animation::domain::choreography::{Cue, Look, Pass};
use crate::animation::domain::motion::accents::Accent;
use crate::animation::domain::motion::score::{Edit, MediaClock};
use crate::animation::platform::overlay::{OverlayTile, TRACE_TARGET, same_layout, window_origin};
use crate::animation::platform::renderer::{Batch, Render};
use crate::decorations::domain::style::Worn;
use crate::pictures::domain::handover::HandoverCheck;
use crate::pictures::domain::route::Route;
use crate::pictures::domain::schedule::{Ask, Display, Hold, Priority};
use crate::pictures::domain::target::PictureTarget;
use crate::pictures::domain::watch::{Client, Moment, Tag, Why};
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
    self, Command, Generation, StageSummary, TileSummary, losing_focus,
};
use crate::transitions::domain::request::{AnimationRequest, Cause, SurfaceWindow};
use crate::transitions::domain::running::{
    Composed, RunningAnimation, Shape, Took, restack, summary,
};
use crate::transitions::domain::timing::*;
use crate::transitions::platform::journal::Writer;
use crate::transitions::settings::JournalSettings;
use crate::windows::platform::window_server::WindowServer;

mod asks;
mod compose;
mod logged;
mod tiles;

use asks::{EndWatches, on_screen, sized_targets};
use compose::{SurfaceFlight, SurfaceMotion};
use tiles::frames_of;

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
    /// The key window changed, which turned these windows' faces, each at the frame it is at. Sent by
    /// the reactor for every change it hears of, rini's own raises' included.
    FacesTurned(Vec<(WindowId, ScreenRect)>),
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
    running: Option<RunningAnimation<Picture>>,
    /// The picture delivered of each window, under its generation and at its frame, while the
    /// coordinator holds it to swap in later (`Coordinator::holds`): a landing held back from over a
    /// focused face until focus hands over, and the target's change until the window focus leaves has
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
                self.start_surface(SurfaceFlight {
                    windows,
                    from_offset,
                    to_offset,
                    final_frames,
                    focus: (focus, focused_before),
                    key,
                    duration,
                    look,
                    cue,
                    motion: SurfaceMotion::Travels(accent),
                })
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
            Event::FacesTurned(windows) => self.drive(lifecycle::Event::FacesTurned(windows)),
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
            Command::KeepFocusedFace { generation, window } => {
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
        match running.join(&tile, viewport, left.as_secs_f64(), at) {
            Some(edits) => self.play(edits),
            None => self.reinstall(),
        }
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
        let score = running.pose();
        let tiles = running.tiles.clone();
        self.render(Render::Stage { score, tiles });
    }

    /// Opens a transition, when none flies, from what a request alone says (`tiles`, read only
    /// then), before anything is composed, so its frame-zero pictures are asked for first. False
    /// when it could not run, and was abandoned.
    fn begin(
        &mut self,
        tiles: impl FnOnce() -> Vec<TileSummary>,
        final_frames: &[(WindowId, ScreenRect)],
        (focus, focused_before): (Option<WindowId>, Option<WindowId>),
        key: Option<WindowId>,
        shape: &Shape,
    ) -> bool {
        if self.flights.coordinator().is_flying() {
            return true;
        }
        let Some((display, _)) = self.display else { return false };
        let stage = summary(
            tiles(),
            final_frames.to_vec(),
            focus,
            losing_focus(focus, focused_before),
            None,
            key,
            shape.clone(),
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

    /// Runs one composed pass: merged into the flight running, or posed as its frame zero.
    /// See "The overlay engine" in `src/animation/docs/animation-smoothness.md`.
    fn begin_group(&mut self, pass: Composed<Picture>) {
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
            logged::dressed(pass.label, &dressing, strip);
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
            focused_face: None,
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
            && self.fly_focused_face(target)
        {
            stage.focused_face = Some(target);
        }
        self.reinstall();
        self.drive(lifecycle::Event::Merge { stage, moved: false });
    }

    /// A focus target parked off the display flies its focused face, if the service holds one that
    /// fits: frame zero cannot picture it, and it arrives focused. Whether it does.
    fn fly_focused_face(&mut self, target: WindowId) -> bool {
        let face = self.pictures.focused(target);
        let clipped = face.as_ref().is_some_and(|face| face.source.clips());
        let worn = face.as_ref().map(|face| (target, face.source, face.carries_blur, face.taken));
        let flown = self
            .running
            .as_mut()
            .is_some_and(|running| running.fly_focused_face(target, face, clipped));
        if flown {
            self.journal_wears(Wearing::FocusedFace, worn.into_iter().collect());
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

    /// On a flight that moves, the tiles a merge turned focused or not drawn so, the focused face a
    /// new parked target flies cut onto its tile, and the ring and border of every tile whose end the
    /// merge changed worn at once, as the overlay showed the first pass's: a retarget draws none.
    fn redraw_focus(&mut self, before: &[(WindowId, (bool, Worn))], face: Option<WindowId>) {
        let Some(running) = self.running.as_ref() else { return };
        let mut drawn = Vec::new();
        for tile in running.tiles.iter() {
            let was = before.iter().find(|(w, _)| *w == tile.window).map(|(_, drawn)| *drawn);
            let (window, focused, worn) = (tile.window, tile.focused, tile.worn);
            if face == Some(window) {
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
    fn merge_group(&mut self, pass: Composed<Picture>) {
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
    /// pass moving focus to a target parked off the display flies its focused face, as the first
    /// pass's does.
    fn merge_now(&mut self, pass: Composed<Picture>) {
        let now = self.now();
        let at = self.media.at(now);
        let left = self.flights.coordinator().remaining(now).unwrap_or_default();
        let viewport = self.viewport();
        let display = self.display.map(|(frame, _)| frame);
        let Some(running) = self.running.as_mut() else { return };
        running.take_ends(pass.entrances, pass.final_frames, display);
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
                logged::merged(&running.plan, running.score.score(), &delta, at);
            }
            moved = delta.moves_anything();
            edits.push(running.bank());
            played = edits;
            // A merge that moves anything restarts the clock, over everything the score plays.
            shape.duration = running.score.clock(shape.duration, at);
        }
        let flown = running.flown;
        let extend = (flown && !moved).then(|| running.score.outlasting(left, at)).flatten();
        let focus_moved = running.focus != focus_before;
        self.journal_wears(Wearing::Compose, composed);
        self.play(played);
        let face = if focus_moved
            && let Some(target) = self.stage(shape.clone()).and_then(|stage| stage.parked_focus())
            && self.fly_focused_face(target)
        {
            Some(target)
        } else {
            None
        };
        if flown {
            self.redraw_focus(&drawn_focused_before, face);
        } else {
            self.reinstall();
        }
        let Some(stage) = self.stage(shape) else { return };
        self.drive(lifecycle::Event::Merge { stage, moved });
        if let Some(extend) = extend {
            self.drive(lifecycle::Event::Extend(extend));
        }
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
            self.start_surface(SurfaceFlight {
                windows,
                from_offset: at_rest,
                to_offset: at_rest,
                final_frames,
                focus: (focus, None),
                key: None,
                duration,
                look,
                cue,
                motion: SurfaceMotion::Bounces,
            });
        }
        let at = self.media.at(self.now());
        let Some(running) = self.running.as_mut() else { return };
        let Some(edits) = running.push(&cue, overshoot, duration.as_secs_f64(), at) else {
            return;
        };
        let remaining = running.score.remaining(at);
        self.play(edits);
        debug!(
            overshoot = %logged::Whole(overshoot),
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
        // The acceptance greps count this line against "overlay lifted".
        let flight = &running.plan;
        debug!(
            groups = flight.groups.iter().filter(|g| !g.members.is_empty()).count(),
            changing = flight.changing.len(),
            entrances = flight.entrances.len(),
            floating = flight.floating.len(),
            "flight composed"
        );
        let edits = running.start_moving(duration.as_secs_f64(), at);
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

#[cfg(test)]
mod tests {
    use rini_geometry::space::{ScreenSize, ScreenVector, SurfaceRect, ToCg};

    use super::*;
    use crate::animation::domain::motion::score::Node;
    use crate::animation::domain::motion::surface::SurfaceGeometry;
    use crate::animation::domain::motion::testing::{DISPLAY, rect};
    use crate::animation::domain::motion::tile;
    use crate::decorations::domain::style::Ring;
    use crate::pictures::domain::schedule::MAX_SYNC_ENTRANCE_CAPTURES;
    use crate::pictures::platform::picture::test_picture;

    /// A frame on `DISPLAY`, which sits at the origin, on the overlay over it.
    fn on(frame: ScreenRect) -> OverlayRect { to_overlay_space(frame, DISPLAY) }

    /// An overlay frame back on `DISPLAY`.
    fn screen(frame: OverlayRect) -> ScreenRect { from_overlay_space(frame, DISPLAY) }

    fn wid(idx: u32) -> WindowId { WindowId::new(7, idx) }

    /// A SkyLight sliver: 40pt of an `size`-wide window, which `is_usable` rejects.
    fn clipped(size: ScreenSize) -> Picture {
        let mut snapshot = test_picture(size);
        snapshot.coverage.covered = ScreenSize::new(40.0, size.height);
        snapshot
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

        /// A merge that moves focus onto a parked window flies its focused face, swapped onto its tile
        /// clipped when the face was baked with the desktop behind it and drawn whole when it was not.
        #[test]
        fn a_parked_targets_focused_face_swaps_in_clipped_by_what_it_is() {
            for (route, clipped) in [(Route::Baked, true), (Route::SkyLight, false)] {
                let mut h = Harness::new();
                let (_, shown) = h.shown();
                h.commit(shown);
                h.advance(Duration::from_millis(50));
                park(&h, 2);
                let mut face = test_picture(end().size);
                face.source = route;
                h.engine.pictures.hold_focused(wid(2), face);
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
                    h.engine.running.as_ref().and_then(|running| running.focused_face),
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
                batches(&h.step(Event::FacesTurned(vec![(wid(1), end())]))).is_empty(),
                "a turned face"
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
