//! The transitions journal: the press each request answers, every command the lifecycle gives and
//! every event it takes but a picture and frame zero, which their delivery marks, a tick only by the
//! first `at_rest` and `landed` it reads in a generation; every delivery of pictures to it, each
//! drain of render commands the renderer committed, each held lift's hide and handover, and each turn
//! of the main thread over a frame. Each is a mark in a ring kept with the coordinator, and a burst of
//! marks is written as JSON lines. While the handover is measured, it also says which lifts are held
//! for the overlay's own picture ([`measures`]) and when one hides. Pure: the engine drives the
//! lifecycle, tells it each request's press, each delivery, each commit, each lift it holds, what that
//! lift hears, each handover and each slow main-thread turn through [`Journalled`], and hands each
//! burst it gives to the writer
//! (`crate::transitions::platform::journal`). The marks, the clock and when a burst is given:
//! `src/transitions/docs/journal.md`.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use rini_core::ids::WindowId;
use rini_geometry::space::{OverlayRect, Rect, ScreenRect};
use serde::{Serialize, Serializer};
use serde_with::{SerializeAs, serde_as};

use super::lifecycle::overview::{self, Overview};
use super::lifecycle::{Arrived, Command, Coordinator, Event, Generation, Kind, Start};
use super::request::Cause;
#[cfg(test)]
use crate::animation::event::Crops;
use crate::animation::event::{OverlayTurn, Played, RestPlacement};
use crate::pictures::domain::handover::HandoverDiff;
use crate::pictures::domain::route::Route;
use crate::pictures::domain::watch::Why;

/// How many marks the ring holds.
pub const RING: usize = 8192;

/// The line format's version, in every burst's header.
const FORMAT: u32 = 1;

/// One step of a transition: the press a request answers, an event the coordinator took, a command
/// it gave, a delivery of pictures to it, a drain the renderer committed, or a held lift's hide or
/// one window of its handover.
#[serde_as]
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
#[serde(tag = "mark", rename_all = "snake_case")]
pub enum Mark {
    /// The event a request answers, stamped when the reactor took it.
    Press {
        what: &'static str,
    },
    Begin {
        kind: Kind,
        coalesced: bool,
        tiles: u16,
        awaiting: u16,
        repainting: u16,
        #[serde_as(as = "Option<PidIdx>")]
        focus: Option<WindowId>,
        #[serde_as(as = "Option<PidIdx>")]
        losing_focus: Option<WindowId>,
        #[serde_as(as = "Option<PidIdx>")]
        key: Option<WindowId>,
        duration_ms: u32,
    },
    Merge {
        kind: Kind,
        moved: bool,
        tiles: u16,
        awaiting: u16,
        repainting: u16,
        #[serde_as(as = "Option<PidIdx>")]
        focus: Option<WindowId>,
        #[serde_as(as = "Option<PidIdx>")]
        #[serde(rename = "focused_look")]
        focused_face: Option<WindowId>,
        duration_ms: u32,
    },
    /// A pass reaching a flight whose motion runs, not merged yet: how many windows it grows, and how
    /// many real windows it sends somewhere.
    HoldMerge {
        grows: u16,
        writes: u16,
    },
    /// The passes held since the motion ran merge, `waited_ms` after the latest of them held for a
    /// grow arrived.
    Retarget {
        waited_ms: u32,
    },
    Extend {
        ms: u32,
    },
    Fit {
        ms: u32,
    },
    Abandon,
    Quiet {
        of: Generation,
    },
    /// Every tile drawn where it ends, first read so in this generation once the clock ran out.
    AtRest,
    /// Every real window where it was written, first read so in this generation once the clock ran
    /// out.
    Landed,
    Lost {
        of: Generation,
        #[serde_as(as = "PidIdx")]
        window: WindowId,
    },
    #[serde(rename = "looks_turned")]
    FacesTurned {
        #[serde_as(as = "PidIdx")]
        window: WindowId,
    },
    /// A delivery under the engine's tag, as the engine heard it: the generation it was for, how many
    /// pictures it brought, and how long before this mark the service sent it.
    Delivery {
        of: Generation,
        why: Why,
        pictures: u16,
        sent_before_us: u32,
    },
    /// A picture of the delivery marked before it: where its window was when it was taken, and how long
    /// before this mark its capture started.
    Delivered {
        #[serde_as(as = "PidIdx")]
        window: WindowId,
        #[serde_as(as = "Xywh")]
        frame: ScreenRect,
        taken_before_us: u32,
    },
    Hold,
    Settle,
    Release,
    AskFrameZero {
        strip: u16,
        background: u16,
        behind: u16,
    },
    Show,
    StartMoving,
    StopWatches,
    Write {
        windows: u16,
    },
    Holes {
        windows: u16,
    },
    Warm {
        windows: u16,
    },
    Refresh {
        windows: u16,
    },
    Watch {
        landing: u16,
        awaiting: u16,
        background: u16,
        #[serde_as(as = "Option<PidIdx>")]
        focus: Option<WindowId>,
        #[serde_as(as = "Option<PidIdx>")]
        losing_focus: Option<WindowId>,
    },
    AskFinal {
        windows: u16,
        behind: u16,
    },
    Swap {
        of: Generation,
        #[serde_as(as = "PidIdx")]
        window: WindowId,
    },
    #[serde(rename = "keep_focused_look")]
    KeepFocusedFace {
        of: Generation,
        #[serde_as(as = "PidIdx")]
        window: WindowId,
    },
    Snap {
        #[serde_as(as = "PidIdx")]
        window: WindowId,
        #[serde_as(as = "Xywh")]
        frame: OverlayRect,
    },
    /// A picture a tile took: as the flight composed it, from frame zero before the show, as its
    /// focused face, or swapped in. Which way the picture was taken, whether it carries its window's
    /// blur, and how long before this mark its capture started.
    Wears {
        #[serde_as(as = "PidIdx")]
        window: WindowId,
        at: Wearing,
        route: Route,
        blur: bool,
        taken_before_us: u32,
    },
    Focus {
        #[serde_as(as = "PidIdx")]
        window: WindowId,
        focused: bool,
    },
    PlaceRest,
    /// What the tick that lifted it read: either false, `LIFT_GRACE` ran out.
    Lift {
        at_rest: bool,
        landed: bool,
    },
    /// The overlay of a lift held for its own picture comes down, `waited_us` after the lift: on that
    /// picture, measured, or not, on word that it was not taken, on a request, on the overlay moving or
    /// on the patience.
    Hide {
        measured: bool,
        waited_us: u32,
    },
    /// A drain of render commands the renderer committed, as the engine heard it: the highest batch
    /// in it, whether it put the overlay up or down (`None`: neither), whether it placed what is drawn
    /// at rest, how many tiles it drew on the crop grid and how long reading their grounds took, how
    /// long its oldest batch waited for the renderer, how long playing it took to its flush, and how
    /// long before this mark it was flushed.
    Drawn {
        seq: u64,
        shown: Option<bool>,
        places: bool,
        crops: u32,
        ground_us: u32,
        queued_us: u32,
        applied_us: u32,
        flushed_before_us: u32,
    },
    /// A window of a measured lift: the screen just after it against the overlay just before.
    Handover {
        of: Generation,
        #[serde_as(as = "PidIdx")]
        window: WindowId,
        interior: f32,
        interior_over: f32,
        edge_in: f32,
        edge_out: f32,
        /// Over the band rini draws outside the window at rest; absent with none.
        #[serde(skip_serializing_if = "Option::is_none")]
        band: Option<f32>,
    },
    ReportUp,
    ReportSettled,
    /// A step of the overview's own lifecycle (`lifecycle::overview`): its open, each scene, its
    /// close, and what each gave that no flight's mark says.
    Overview {
        step: OverviewStep,
    },
    /// A turn of the main thread's run loop over a frame at 120Hz, stamped when it began: how long it
    /// ran, the time no actor's poll covers, and how many `turn_actor` and `turn_change` marks follow.
    MainTurn {
        busy_us: u32,
        outside_us: u32,
        actors: u16,
        changes: u16,
    },
    /// An actor polled in the turn marked before it, and how long its polls took.
    TurnActor {
        actor: &'static str,
        us: u32,
    },
    /// A window change made from 50ms before the turn marked before it began, and how long before it
    /// began: negative, made in it.
    TurnChange {
        what: &'static str,
        before_us: i32,
    },
}

/// A step of the overview's lifecycle an `overview` mark records.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OverviewStep {
    /// Opened, or opened again as it closed.
    Open,
    /// A scene laid out after a batch reached it.
    Scene,
    Close,
    /// Frame zero asked of the windows the display shows.
    AskFrameZero,
    /// A scene played from where everything is drawn.
    Played,
    /// The zoom back in begun.
    Exit,
    /// The zoom out again begun, opened as it closed.
    Turn,
    /// The flights held while it was up let go.
    Replay,
}

/// When a tile took the picture a `wears` mark records.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Wearing {
    /// The flight composed the tile with the cache's picture, or the stand-in.
    Compose,
    /// Frame zero's picture, put on before the overlay was up.
    FrameZero,
    /// The parked focus target's focused face.
    #[serde(rename = "focused_look")]
    FocusedFace,
    /// A delivered picture swapped onto the tile, or an entrance joining with its first.
    Swap,
    /// A delivered picture held for the corner of the tile's leg in two steps, on from there.
    Corner,
}

/// A mark, when its drive ran and the generation that drive left.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Stamped {
    pub at: Instant,
    pub generation: Generation,
    pub mark: Mark,
}

static_assertions::const_assert!(std::mem::size_of::<Stamped>() <= 80);

/// Every mark since the last write, oldest first, and how many the ring overwrote before they were
/// written.
#[derive(Debug, PartialEq)]
pub struct Burst {
    pub marks: Vec<Stamped>,
    pub lost: u64,
}

/// An instant and the uptime clock's reading at it, which turn any instant near it into the probe's
/// nanoseconds.
#[derive(Clone, Copy, Debug)]
pub struct Anchor {
    pub at: Instant,
    pub uptime_ns: u64,
}

impl Anchor {
    pub fn ns(&self, at: Instant) -> u64 {
        match at.checked_duration_since(self.at) {
            Some(after) => self.uptime_ns.saturating_add(nanos(after)),
            None => self.uptime_ns.saturating_sub(nanos(self.at - at)),
        }
    }
}

/// A burst as it is written: its header, then one line a mark, oldest first.
pub fn lines<'a>(
    anchor: &'a Anchor,
    unix_ms: u64,
    burst: &'a Burst,
) -> impl Iterator<Item = String> + 'a {
    std::iter::once(header(anchor, unix_ms, burst))
        .chain(burst.marks.iter().map(|stamped| line(anchor, stamped)))
}

/// The line ahead of a burst's marks: when it was written, on both clocks, and how many marks it lost.
fn header(anchor: &Anchor, unix_ms: u64, burst: &Burst) -> String {
    #[derive(Serialize)]
    struct Header {
        t: u64,
        mark: &'static str,
        journal: u32,
        unix_ms: u64,
        lost: u64,
    }
    json(&Header {
        t: anchor.uptime_ns,
        mark: "burst",
        journal: FORMAT,
        unix_ms,
        lost: burst.lost,
    })
}

fn line(anchor: &Anchor, stamped: &Stamped) -> String {
    #[derive(Serialize)]
    struct Line<'a> {
        t: u64,
        g: Generation,
        #[serde(flatten)]
        mark: &'a Mark,
    }
    json(&Line {
        t: anchor.ns(stamped.at),
        g: stamped.generation,
        mark: &stamped.mark,
    })
}

fn json(value: &impl Serialize) -> String {
    serde_json::to_string(value).expect("a mark is numbers, flags and names")
}

/// A window as `[pid, idx]`.
struct PidIdx;

impl SerializeAs<WindowId> for PidIdx {
    fn serialize_as<S: Serializer>(window: &WindowId, serializer: S) -> Result<S::Ok, S::Error> {
        (window.pid, window.idx.get()).serialize(serializer)
    }
}

/// A rect in any space as `[x, y, w, h]`.
struct Xywh;

impl<U> SerializeAs<Rect<U>> for Xywh {
    fn serialize_as<S: Serializer>(rect: &Rect<U>, serializer: S) -> Result<S::Ok, S::Error> {
        let (origin, size) = (rect.origin, rect.size);
        [
            origin.x as f32,
            origin.y as f32,
            size.width as f32,
            size.height as f32,
        ]
        .serialize(serializer)
    }
}

fn nanos(span: Duration) -> u64 { u64::try_from(span.as_nanos()).unwrap_or(u64::MAX) }

fn millis(span: Duration) -> u32 { u32::try_from(span.as_millis()).unwrap_or(u32::MAX) }

fn micros(span: Duration) -> u32 { u32::try_from(span.as_micros()).unwrap_or(u32::MAX) }

/// `later - earlier` in microseconds, either way round.
fn signed_micros(earlier: Instant, later: Instant) -> i32 {
    match later.checked_duration_since(earlier) {
        Some(span) => i32::try_from(span.as_micros()).unwrap_or(i32::MAX),
        None => i32::try_from((earlier - later).as_micros()).map_or(i32::MIN, |us| -us),
    }
}

fn count(n: usize) -> u16 { u16::try_from(n).unwrap_or(u16::MAX) }

/// An event's marks, read before the coordinator takes it.
enum Pending {
    Nothing,
    One(Mark),
    /// A tick's reading, taken only once the clock has run out: before that the engine reads nothing.
    Tick(Arrived),
    FacesTurned(Vec<WindowId>),
}

/// The generation `at_rest` and `landed` were each last marked in.
#[derive(Default)]
struct Edges {
    at_rest: Option<Generation>,
    landed: Option<Generation>,
}

/// Whether `read` is true for the first time in `generation`, `seen` being the generation it was last
/// marked in.
fn first_in(seen: &mut Option<Generation>, read: bool, generation: Generation) -> bool {
    let first = read && *seen != Some(generation);
    if first {
        *seen = Some(generation);
    }
    first
}

/// The marks in a ring of [`RING`], and how far they have been written.
struct Journal {
    ring: VecDeque<Stamped>,
    /// The number of the oldest mark held, counting every mark ever pushed...
    first: u64,
    /// ... and of the oldest one not written.
    written: u64,
    edges: Edges,
}

impl Default for Journal {
    fn default() -> Self {
        Journal {
            ring: VecDeque::with_capacity(RING),
            first: 0,
            written: 0,
            edges: Edges::default(),
        }
    }
}

impl Journal {
    /// `reads_landing`: the coordinator's `wants_landing` as the event comes, the only ticks whose
    /// readings the engine took.
    fn before(event: &Event, reads_landing: bool) -> Pending {
        match event {
            Event::Begin(stage) => Pending::One(Mark::Begin {
                kind: stage.kind,
                coalesced: stage.start == Start::Coalesced,
                tiles: count(stage.tiles.len()),
                awaiting: count(stage.waits.awaiting().count()),
                repainting: count(stage.waits.repainting().count()),
                focus: stage.focus,
                losing_focus: stage.losing_focus,
                key: stage.key,
                duration_ms: millis(stage.duration),
            }),
            Event::Merge { stage, moved } => Pending::One(Mark::Merge {
                kind: stage.kind,
                moved: *moved,
                tiles: count(stage.tiles.len()),
                awaiting: count(stage.waits.awaiting().count()),
                repainting: count(stage.waits.repainting().count()),
                focus: stage.focus,
                focused_face: stage.focused_face,
                duration_ms: millis(stage.duration),
            }),
            Event::HoldMerge { grows, writes } => Pending::One(Mark::HoldMerge {
                grows: count(grows.len()),
                writes: count(writes.len()),
            }),
            Event::FrameZero(_) | Event::Picture { .. } => Pending::Nothing,
            Event::Lost { generation, window } => Pending::One(Mark::Lost {
                of: *generation,
                window: *window,
            }),
            Event::Extend(needs) => Pending::One(Mark::Extend { ms: millis(*needs) }),
            Event::Fit(plays) => Pending::One(Mark::Fit { ms: millis(*plays) }),
            Event::Tick { settled, landed } if reads_landing => Pending::Tick(Arrived {
                settled: *settled,
                landed: *landed,
            }),
            Event::Tick { .. } => Pending::Nothing,
            Event::Quiet(of) => Pending::One(Mark::Quiet { of: *of }),
            Event::Abandon => Pending::One(Mark::Abandon),
            Event::FacesTurned(windows) => {
                Pending::FacesTurned(windows.iter().map(|(window, _)| *window).collect())
            }
        }
    }

    /// The event's marks and one per command, stamped `now` and the generation the drive left.
    fn after(
        &mut self,
        now: Instant,
        generation: Generation,
        pending: Pending,
        commands: &[Command],
    ) {
        let mut read = Arrived::default();
        match pending {
            Pending::Nothing => {}
            Pending::One(mark) => self.push(now, generation, mark),
            Pending::Tick(arrived) => {
                read = arrived;
                if first_in(&mut self.edges.at_rest, arrived.settled, generation) {
                    self.push(now, generation, Mark::AtRest);
                }
                if first_in(&mut self.edges.landed, arrived.landed, generation) {
                    self.push(now, generation, Mark::Landed);
                }
            }
            Pending::FacesTurned(windows) => {
                for window in windows {
                    self.push(now, generation, Mark::FacesTurned { window });
                }
            }
        }
        for command in commands {
            self.push(now, generation, Self::mark(command, read));
        }
    }

    /// `read`: what the drive's tick read, which a lift carries.
    fn mark(command: &Command, read: Arrived) -> Mark {
        match command {
            Command::Hold => Mark::Hold,
            Command::Settle => Mark::Settle,
            Command::Release => Mark::Release,
            Command::AskFrameZero { strip, background, behind, .. } => Mark::AskFrameZero {
                strip: count(strip.len()),
                background: count(background.len()),
                behind: count(behind.len()),
            },
            Command::Show => Mark::Show,
            Command::Write(frames) => Mark::Write { windows: count(frames.len()) },
            Command::Holes(holes) => Mark::Holes { windows: count(holes.len()) },
            Command::Watch(watch) => Mark::Watch {
                landing: count(watch.landing.len()),
                awaiting: count(watch.awaiting.len()),
                background: count(watch.background.len()),
                focus: watch.focus.map(|(window, _)| window),
                losing_focus: watch.losing_focus.map(|(window, _)| window),
            },
            Command::StopWatches => Mark::StopWatches,
            Command::StartMoving => Mark::StartMoving,
            Command::Retarget { held } => Mark::Retarget { waited_ms: millis(*held) },
            Command::AskFinal { windows, behind, .. } => Mark::AskFinal {
                windows: count(windows.len()),
                behind: count(behind.len()),
            },
            Command::Swap { generation, window } => Mark::Swap {
                of: *generation,
                window: *window,
            },
            Command::Snap { window, frame } => Mark::Snap { window: *window, frame: *frame },
            Command::KeepFocusedFace { generation, window } => Mark::KeepFocusedFace {
                of: *generation,
                window: *window,
            },
            Command::Focus { window, focused } => Mark::Focus {
                window: *window,
                focused: *focused,
            },
            Command::PlaceRest => Mark::PlaceRest,
            Command::Lift { .. } => Mark::Lift {
                at_rest: read.settled,
                landed: read.landed,
            },
            Command::Warm(windows) => Mark::Warm { windows: count(windows.len()) },
            Command::Refresh(windows) => Mark::Refresh { windows: count(windows.len()) },
            Command::ReportUp => Mark::ReportUp,
            Command::ReportSettled => Mark::ReportSettled,
        }
    }

    /// A delivery's mark, then one for each of its pictures, all stamped `now`, when the engine heard
    /// it, and the generation the coordinator is in.
    fn delivery(
        &mut self,
        now: Instant,
        generation: Generation,
        of: Generation,
        why: Why,
        sent: Instant,
        pictures: impl ExactSizeIterator<Item = (WindowId, ScreenRect, Instant)>,
    ) {
        let before = |at: Instant| micros(now.saturating_duration_since(at));
        let delivery = Mark::Delivery {
            of,
            why,
            pictures: count(pictures.len()),
            sent_before_us: before(sent),
        };
        self.push(now, generation, delivery);
        for (window, frame, taken) in pictures {
            self.push(now, generation, Mark::Delivered {
                window,
                frame,
                taken_before_us: before(taken),
            });
        }
    }

    /// A request's press, stamped when the reactor took it and with the generation its first drive
    /// left.
    fn press(&mut self, cause: Cause, generation: Generation) {
        self.push(cause.at, generation, Mark::Press { what: cause.what });
    }

    /// One window of the handover of `of`'s lift, stamped `now`, when the engine heard it.
    fn handover(
        &mut self,
        now: Instant,
        generation: Generation,
        of: Generation,
        window: WindowId,
        diff: HandoverDiff,
    ) {
        let HandoverDiff {
            interior,
            interior_over,
            edge_in,
            edge_out,
            band,
        } = diff;
        self.push(now, generation, Mark::Handover {
            of,
            window,
            interior,
            interior_over,
            edge_in,
            edge_out,
            band,
        });
    }

    fn push(&mut self, at: Instant, generation: Generation, mark: Mark) {
        if self.ring.len() == RING {
            self.ring.pop_front();
            self.first += 1;
        }
        self.ring.push_back(Stamped { at, generation, mark });
    }

    /// Writing turns on: the cursor moves up to the oldest mark held, so what the ring overwrote while
    /// nothing was written is not counted lost.
    fn start_writing(&mut self) { self.written = self.written.max(self.first); }

    fn unwritten(&mut self) -> Option<Burst> {
        let end = self.first + self.ring.len() as u64;
        let from = self.written.max(self.first);
        let lost = from - self.written;
        if from == end && lost == 0 {
            return None;
        }
        let marks = self.ring.range((from - self.first) as usize..).copied().collect();
        self.written = end;
        Some(Burst { marks, lost })
    }
}

/// The lifecycle with its journal, driven as one: the press each request answers, every command the
/// coordinator gives, every event it takes but a picture and frame zero, a tick only by the first
/// `at_rest` and `landed` it reads in a generation, every delivery of pictures the engine hears and
/// every commit the renderer reports and every slow turn of the main thread is marked, and while the
/// journal is written a burst is given
/// once it settles. While the handover is measured, it decides when the lift the engine holds hides,
/// and marks its hide and its handover.
#[derive(Default)]
pub struct Journalled {
    coordinator: Coordinator,
    overview: Overview,
    journal: Journal,
    writing: bool,
    /// Between a `ReportUp` and its `ReportSettled`, when nothing is written.
    up: bool,
    /// The press of the request being handled, until its first drive marks it.
    pressed: Option<Cause>,
    /// The lift held for the overlay's own picture, until it hides.
    waiting: Option<LiftWait>,
    /// The last held lift, when it hid on its picture: the one whose handover is marked.
    measured: Option<Generation>,
}

impl Journalled {
    pub fn coordinator(&self) -> &Coordinator { &self.coordinator }

    pub fn overview(&self) -> &Overview { &self.overview }

    /// A generation of the overview's own, taken while no flight flies (`Coordinator::reserve`).
    pub fn reserve(&mut self) -> Generation { self.coordinator.reserve() }

    /// Tells the overview's lifecycle `event`, as `drive` tells the coordinator one: the press the
    /// request carried, the event and each command marked, under the overview's generation, and the
    /// burst to write when it settles.
    pub fn drive_overview(
        &mut self,
        event: overview::Event,
        now: Instant,
    ) -> (Vec<overview::Command>, Option<Burst>) {
        let read = match event {
            overview::Event::Tick { settled, landed } => Arrived { settled, landed },
            _ => Arrived::default(),
        };
        let commands = self.overview.on(event, now);
        let generation = self.overview.generation();
        if let Some(cause) = self.pressed.take() {
            self.journal.press(cause, generation);
        }
        let step = match event {
            overview::Event::Open { .. } => Some(OverviewStep::Open),
            overview::Event::Scene => Some(OverviewStep::Scene),
            overview::Event::Close => Some(OverviewStep::Close),
            _ => None,
        };
        if let Some(step) = step {
            self.journal.push(now, generation, Mark::Overview { step });
        }
        if let overview::Event::Quiet(of) = event
            && !commands.is_empty()
        {
            self.journal.push(now, generation, Mark::Quiet { of });
        }
        for command in &commands {
            if let Some(mark) = overview_mark(*command, read) {
                self.journal.push(now, generation, mark);
            }
        }
        let settled = commands.contains(&overview::Command::ReportSettled);
        self.up = (self.up || commands.contains(&overview::Command::ReportUp)) && !settled;
        let burst = if self.writing && settled {
            self.journal.unwritten()
        } else {
            None
        };
        (commands, burst)
    }

    /// Tells the coordinator `event`: the commands to carry out, in order, and the burst to write
    /// when one is due.
    pub fn drive(&mut self, event: Event, now: Instant) -> (Vec<Command>, Option<Burst>) {
        let pending = Journal::before(&event, self.coordinator.wants_landing(now));
        let commands = self.coordinator.on(event, now);
        let generation = self.coordinator.generation();
        if let Some(cause) = self.pressed.take() {
            self.journal.press(cause, generation);
        }
        self.journal.after(now, generation, pending, &commands);
        let settled = commands.contains(&Command::ReportSettled);
        self.up = (self.up || commands.contains(&Command::ReportUp)) && !settled;
        let burst = if self.writing && settled {
            self.journal.unwritten()
        } else {
            None
        };
        (commands, burst)
    }

    /// The engine takes a request answering `cause`: its next drive marks the press, ahead of that
    /// event's own marks.
    pub fn pressed(&mut self, cause: Cause) { self.pressed = Some(cause); }

    /// The engine is done with the request: a press no drive marked, a request it declined, is
    /// dropped.
    pub fn answered(&mut self) { self.pressed = None; }

    /// A delivery of the pictures service under the engine's tag, heard `now`, before the engine turns
    /// it into events: `of` the generation it was for, `sent` when the service sent it, and each picture's
    /// window, the frame it was taken at and when its capture started.
    pub fn delivery(
        &mut self,
        now: Instant,
        of: Generation,
        why: Why,
        sent: Instant,
        pictures: impl ExactSizeIterator<Item = (WindowId, ScreenRect, Instant)>,
    ) {
        self.journal
            .delivery(now, self.coordinator.generation(), of, why, sent, pictures);
    }

    /// `window`'s tile took a picture `at` that moment of the flight, heard `now`: taken by `route`,
    /// carrying its window's blur or not (`blur`), its capture started at `taken`.
    pub fn wears(
        &mut self,
        now: Instant,
        window: WindowId,
        at: Wearing,
        route: Route,
        blur: bool,
        taken: Instant,
    ) {
        let taken_before_us = micros(now.saturating_duration_since(taken));
        let mark = Mark::Wears {
            window,
            at,
            route,
            blur,
            taken_before_us,
        };
        self.journal.push(now, self.coordinator.generation(), mark);
    }

    /// The renderer committed the drain up to batch `seq`, heard `now`, before what it releases is
    /// carried out: what it `played`, `sent` when its oldest batch was sent, `began` when the
    /// renderer began playing it and `flushed` when it was flushed.
    pub fn drawn(
        &mut self,
        now: Instant,
        seq: u64,
        played: Played,
        (sent, began, flushed): (Instant, Instant, Instant),
    ) {
        // transitions.jsonl keeps `shown` and `places` as booleans, which journal.py reads.
        let mark = Mark::Drawn {
            seq,
            shown: played.turn.map(|turn| turn == OverlayTurn::Shown),
            places: played.rest == RestPlacement::Placed,
            crops: played.crops.fills,
            ground_us: micros(played.crops.ground),
            queued_us: micros(began.saturating_duration_since(sent)),
            applied_us: micros(flushed.saturating_duration_since(began)),
            flushed_before_us: micros(now.saturating_duration_since(flushed)),
        };
        self.journal.push(now, self.coordinator.generation(), mark);
    }

    /// A turn of the main thread over a frame, from `began` for `busy`: each actor polled in it with
    /// the time its polls took, the time outside them, and each window change from 50ms before it
    /// began, with when. Stamped when it began, in the generation the coordinator is in, so it is
    /// written with the burst it falls in.
    pub fn main_turn(
        &mut self,
        began: Instant,
        busy: Duration,
        actors: &[(&'static str, Duration)],
        outside: Duration,
        changes: &[(&'static str, Instant)],
    ) {
        let generation = self.coordinator.generation();
        let turn = Mark::MainTurn {
            busy_us: micros(busy),
            outside_us: micros(outside),
            actors: count(actors.len()),
            changes: count(changes.len()),
        };
        self.journal.push(began, generation, turn);
        for &(actor, took) in actors {
            self.journal
                .push(began, generation, Mark::TurnActor { actor, us: micros(took) });
        }
        for &(what, at) in changes {
            self.journal.push(began, generation, Mark::TurnChange {
                what,
                before_us: signed_micros(at, began),
            });
        }
    }

    /// The engine holds `generation`'s lift, from `now`, for the overlay's own picture.
    pub fn lifted(&mut self, generation: Generation, now: Instant) {
        self.waiting = Some(LiftWait { generation, since: now });
    }

    /// What the held lift hears at `now`: whether it hides now, its `hide` marked. Only a lift that
    /// hid on its own picture is measured.
    pub fn heard(&mut self, heard: Heard, now: Instant) -> bool {
        let Some(waiting) = self.waiting else { return false };
        let Some(hide) = hides(waiting, heard, now) else {
            return false;
        };
        self.waiting = None;
        let measured = hide == Hide::Measured;
        self.measured = measured.then_some(waiting.generation);
        let waited_us = micros(now.saturating_duration_since(waiting.since));
        self.journal.push(now, self.coordinator.generation(), Mark::Hide {
            measured,
            waited_us,
        });
        true
    }

    /// The handover of `of`'s lift, as the pictures service compared it, heard `now`: one mark a
    /// window when that lift hid on its own picture, and none when it did not, since the overlay may
    /// have been gone from its picture.
    pub fn handover(
        &mut self,
        now: Instant,
        of: Generation,
        windows: impl IntoIterator<Item = (WindowId, HandoverDiff)>,
    ) {
        if self.measured != Some(of) {
            return;
        }
        self.measured = None;
        let generation = self.coordinator.generation();
        for (window, diff) in windows {
            self.journal.handover(now, generation, of, window, diff);
        }
    }

    /// Writing turned on or off. Turned on with no burst up, it gives what the ring holds at once; in
    /// a burst, that burst's settle gives it.
    pub fn set_writing(&mut self, on: bool) -> Option<Burst> {
        let starting = on && !self.writing;
        self.writing = on;
        if !starting {
            return None;
        }
        self.journal.start_writing();
        if self.up {
            None
        } else {
            self.journal.unwritten()
        }
    }
}

/// The mark an overview's command leaves, a flight's where a flight gives the same; none for its
/// clock, its bands or what it forgets. `read`: what the tick that gave it read, which a lift
/// carries.
fn overview_mark(command: overview::Command, read: Arrived) -> Option<Mark> {
    use overview::Command as Overview;
    let step = |step| Some(Mark::Overview { step });
    match command {
        Overview::Hold => Some(Mark::Hold),
        Overview::AskFrameZero(_) => step(OverviewStep::AskFrameZero),
        Overview::Show => Some(Mark::Show),
        Overview::Release => Some(Mark::Release),
        Overview::Play => step(OverviewStep::Played),
        Overview::Exit => step(OverviewStep::Exit),
        Overview::Turn => step(OverviewStep::Turn),
        Overview::PlaceRest => Some(Mark::PlaceRest),
        Overview::Lift(_) => Some(Mark::Lift {
            at_rest: read.settled,
            landed: read.landed,
        }),
        Overview::Settle => Some(Mark::Settle),
        Overview::ReportUp => Some(Mark::ReportUp),
        Overview::ReportSettled => Some(Mark::ReportSettled),
        Overview::Clock(_) | Overview::Widen(_) | Overview::Forget => None,
        Overview::Replay => step(OverviewStep::Replay),
    }
}

/// How long a held lift waits for the overlay's own picture before it hides unmeasured: past a cold
/// SkyLight call (`src/transitions/docs/journal.md`, "The handover, measured").
pub const OVERLAY_TAKE_PATIENCE: Duration = Duration::from_millis(60);

/// Whether a lift is held for the overlay's own picture: while the handover is measured, and with a
/// window ending wholly on screen to compare (`EndWatch::compared`).
pub fn measures(measure_handover: bool, ends: &[(WindowId, ScreenRect)]) -> bool {
    measure_handover && !ends.is_empty()
}

/// A lift held for the overlay's own picture: the generation that lifted, and when.
#[derive(Clone, Copy, Debug, PartialEq)]
struct LiftWait {
    generation: Generation,
    since: Instant,
}

/// What a held lift hears.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Heard {
    /// The pictures service took the overlay's picture for this generation's lift, or did not.
    OverlayTaken { generation: Generation, taken: bool },
    /// A request: a press never waits for the overlay's picture.
    Request,
    /// The overlay is about to be moved to another display, or laid out for another band or scale:
    /// it would move with the finished flight's tiles on it.
    Moved,
    /// The patience's timer.
    Patience,
}

/// How a held lift hides.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Hide {
    /// On its own overlay picture: its handover is marked when it comes.
    Measured,
    /// On word that its picture was not taken, on a request, on the overlay moving, or once the
    /// patience ran out: the overlay may be gone from a picture taken later, so the lift is not
    /// measured.
    Unmeasured,
}

/// Whether the lift `waiting` hides on what it `heard` at `now`, and how.
fn hides(waiting: LiftWait, heard: Heard, now: Instant) -> Option<Hide> {
    match heard {
        Heard::OverlayTaken { generation, taken } => {
            (generation == waiting.generation).then_some(if taken {
                Hide::Measured
            } else {
                Hide::Unmeasured
            })
        }
        Heard::Request | Heard::Moved => Some(Hide::Unmeasured),
        Heard::Patience => (now.saturating_duration_since(waiting.since) >= OVERLAY_TAKE_PATIENCE)
            .then_some(Hide::Unmeasured),
    }
}

#[cfg(test)]
mod tests {
    use rini_geometry::space::{Point, Size};

    use super::*;
    use crate::pictures::domain::route::Route;
    use crate::transitions::domain::lifecycle::EndWatch;
    use crate::transitions::domain::lifecycle::testing::{PLAYS, pan, r, w};
    use crate::transitions::domain::timing::{LIFT_GRACE, SETTLE_BEFORE_CAPTURES};

    fn name(mark: &Mark) -> String {
        serde_json::to_value(mark).unwrap()["mark"].as_str().unwrap().to_owned()
    }

    fn names(marks: &[Stamped]) -> Vec<String> {
        marks.iter().map(|stamped| name(&stamped.mark)).collect()
    }

    /// The marks an event leaves on an empty journal, with no command after it.
    fn marks_of(event: Event) -> Vec<String> {
        let mut journal = Journal::default();
        let pending = Journal::before(&event, true);
        journal.after(Instant::now(), Generation(1), pending, &[]);
        names(journal.ring.make_contiguous())
    }

    fn extend(ms: u32) -> Mark { Mark::Extend { ms } }

    /// Every mark the journalled lifecycle holds.
    fn held(flights: &Journalled) -> Vec<Stamped> { flights.journal.ring.iter().copied().collect() }

    /// A press the reactor took `before` `at`.
    fn pressed(at: Instant, before: Duration) -> Cause {
        Cause {
            at: at - before,
            what: "move_focus",
        }
    }

    fn presses(marks: &[Stamped]) -> Vec<Stamped> {
        marks
            .iter()
            .copied()
            .filter(|stamped| matches!(stamped.mark, Mark::Press { .. }))
            .collect()
    }

    /// The pan from its press to its quiet, landing as soon as the clock runs out: every burst each
    /// drive gave, in order.
    fn fly(flights: &mut Journalled, now: Instant) -> Vec<Option<Burst>> {
        flights.pressed(pressed(now, Duration::from_millis(3)));
        let mut bursts = vec![flights.drive(Event::Begin(pan()), now).1];
        flights.answered();
        let g = flights.coordinator().generation();
        bursts.push(flights.drive(Event::FrameZero(g), now).1);
        bursts.push(flights.drive(Event::Tick { settled: true, landed: true }, now + PLAYS).1);
        bursts.push(flights.drive(Event::Quiet(g), now + PLAYS + SETTLE_BEFORE_CAPTURES).1);
        bursts
    }

    #[test]
    fn every_command_has_its_mark() {
        let g = Generation(3);
        let frames = vec![(w(1), r(0.0, 0.0, 200.0, 200.0))];
        let watch = EndWatch {
            generation: g,
            compared: frames.clone(),
            landing: frames.clone(),
            awaiting: Vec::new(),
            landing_until: Instant::now(),
            awaiting_until: Instant::now(),
            focus: Some((w(2), r(0.0, 0.0, 200.0, 200.0))),
            losing_focus: Some((w(1), r(200.0, 0.0, 200.0, 200.0))),
            background: Vec::new(),
            background_initial: None,
            behind: vec![w(9)],
        };
        let snap = OverlayRect::new(Point::new(1.0, 2.0), Size::new(3.0, 4.0));
        let table = [
            (Command::Hold, "hold"),
            (Command::Settle, "settle"),
            (Command::Release, "release"),
            (
                Command::AskFrameZero {
                    generation: g,
                    strip: frames.clone(),
                    background: Vec::new(),
                    behind: Vec::new(),
                },
                "ask_frame_zero",
            ),
            (Command::Show, "show"),
            (Command::Write(frames.clone()), "write"),
            (Command::Holes(vec![(w(1), snap)]), "holes"),
            (Command::Watch(watch.clone()), "watch"),
            (Command::StopWatches, "stop_watches"),
            (Command::StartMoving, "start_moving"),
            (
                Command::Retarget {
                    held: Duration::from_millis(140),
                },
                "retarget",
            ),
            (
                Command::AskFinal {
                    generation: g,
                    windows: frames.clone(),
                    behind: Vec::new(),
                },
                "ask_final",
            ),
            (Command::Swap { generation: g, window: w(1) }, "swap"),
            (Command::Snap { window: w(1), frame: snap }, "snap"),
            (
                Command::KeepFocusedFace { generation: g, window: w(1) },
                "keep_focused_look",
            ),
            (Command::Focus { window: w(1), focused: false }, "focus"),
            (Command::PlaceRest, "place_rest"),
            (Command::Lift { generation: g }, "lift"),
            (Command::Warm(frames.clone()), "warm"),
            (Command::Refresh(frames), "refresh"),
            (Command::ReportUp, "report_up"),
            (Command::ReportSettled, "report_settled"),
        ];
        for (command, expected) in table {
            assert_eq!(
                name(&Journal::mark(&command, Arrived {
                    settled: true,
                    landed: false
                })),
                expected,
                "{command:?}"
            );
        }
        assert_eq!(
            Journal::mark(&Command::Watch(watch), Arrived::default()),
            Mark::Watch {
                landing: 1,
                awaiting: 0,
                background: 0,
                focus: Some(w(2)),
                losing_focus: Some(w(1))
            }
        );
        assert_eq!(
            Journal::mark(&Command::Lift { generation: g }, Arrived {
                settled: true,
                landed: false
            }),
            Mark::Lift { at_rest: true, landed: false }
        );
        assert_eq!(
            Journal::mark(
                &Command::Retarget {
                    held: Duration::from_micros(140_900)
                },
                Arrived::default()
            ),
            Mark::Retarget { waited_ms: 140 }
        );
    }

    #[test]
    fn every_event_but_picture_and_frame_zero_has_its_mark() {
        let g = Generation(1);
        let frame = r(0.0, 0.0, 200.0, 200.0);
        assert_eq!(marks_of(Event::Begin(pan())), ["begin"]);
        assert_eq!(marks_of(Event::Merge { stage: pan(), moved: true }), ["merge"]);
        let hold = Event::HoldMerge {
            grows: vec![(w(2), frame)],
            writes: vec![(w(1), frame), (w(2), frame)],
        };
        assert_eq!(marks_of(hold.clone()), ["hold_merge"]);
        let Pending::One(held) = Journal::before(&hold, false) else {
            panic!()
        };
        assert_eq!(held, Mark::HoldMerge { grows: 1, writes: 2 });
        assert_eq!(marks_of(Event::Lost { generation: g, window: w(1) }), ["lost"]);
        assert_eq!(marks_of(Event::Extend(Duration::from_millis(5))), ["extend"]);
        assert_eq!(marks_of(Event::Fit(Duration::from_millis(5))), ["fit"]);
        assert_eq!(marks_of(Event::Tick { settled: true, landed: true }), [
            "at_rest", "landed"
        ]);
        assert_eq!(marks_of(Event::Quiet(g)), ["quiet"]);
        assert_eq!(marks_of(Event::Abandon), ["abandon"]);
        assert_eq!(
            marks_of(Event::FacesTurned(vec![(w(1), frame), (w(2), frame)])),
            ["looks_turned", "looks_turned"]
        );
        assert!(
            marks_of(Event::FrameZero(g)).is_empty(),
            "the delivery is the record"
        );
        assert!(
            marks_of(Event::Picture {
                generation: g,
                window: w(1),
                frame,
                why: Why::Landed
            })
            .is_empty()
        );

        let Pending::One(begin) = Journal::before(&Event::Begin(pan()), false) else {
            panic!()
        };
        assert_eq!(begin, Mark::Begin {
            kind: Kind::Pan,
            coalesced: false,
            tiles: 3,
            awaiting: 0,
            repainting: 0,
            focus: Some(w(2)),
            losing_focus: Some(w(1)),
            key: Some(w(1)),
            duration_ms: 350,
        });
    }

    #[test]
    fn a_tick_marks_landed_and_at_rest_once_per_generation() {
        let now = Instant::now();
        let mut flights = Journalled::default();
        flights.drive(Event::Begin(pan()), now);
        flights.drive(Event::FrameZero(flights.coordinator().generation()), now);
        flights.drive(
            Event::Tick { settled: false, landed: false },
            now + Duration::from_millis(240),
        );
        let before = held(&flights).len();
        let end = now + PLAYS;
        flights.drive(Event::Tick { settled: true, landed: false }, end);
        flights.drive(
            Event::Tick { settled: true, landed: false },
            end + Duration::from_millis(16),
        );
        flights.drive(
            Event::Tick { settled: true, landed: true },
            end + Duration::from_millis(32),
        );
        let marks = held(&flights);
        assert_eq!(names(&marks[before..]), [
            "at_rest",
            "landed",
            "place_rest",
            "stop_watches",
            "lift",
            "settle"
        ]);
        assert_eq!(marks[before].at, end, "stamped with the tick that first read it");
    }

    #[test]
    fn a_tick_before_the_clock_runs_out_marks_nothing() {
        let now = Instant::now();
        let mut flights = Journalled::default();
        flights.drive(Event::Begin(pan()), now);
        flights.drive(Event::FrameZero(flights.coordinator().generation()), now);
        let before = held(&flights).len();
        flights.drive(
            Event::Tick { settled: true, landed: true },
            now + Duration::from_millis(10),
        );
        assert_eq!(
            held(&flights).len(),
            before,
            "the engine read nothing, whatever the tick says"
        );
    }

    /// A pass growing a window while the flight moves is marked as it is held, and again as it merges,
    /// with how long it waited, in the generation it was held in.
    #[test]
    fn a_held_merge_is_marked_held_then_merged_with_its_wait() {
        let now = Instant::now();
        let mut flights = Journalled::default();
        let mut stage = pan();
        stage.kind = Kind::LayoutPass;
        flights.drive(Event::Begin(stage), now);
        flights.drive(Event::FrameZero(flights.coordinator().generation()), now);
        let g = flights.coordinator().generation();
        let before = held(&flights).len();
        let grown = r(3.0, 35.0, 1723.0, 1079.0);
        let arrives = now + Duration::from_millis(100);
        flights.drive(
            Event::HoldMerge {
                grows: vec![(w(2), grown)],
                writes: vec![(w(2), grown)],
            },
            arrives,
        );
        flights.drive(
            Event::Picture {
                generation: g,
                window: w(2),
                frame: grown,
                why: Why::Landed,
            },
            arrives + Duration::from_millis(137),
        );
        let marks = held(&flights);
        assert_eq!(names(&marks[before..]), [
            "hold_merge",
            "write",
            "holes",
            "watch",
            "retarget"
        ]);
        assert_eq!(marks[before].mark, Mark::HoldMerge { grows: 1, writes: 1 });
        assert_eq!(
            marks.last().map(|stamped| (stamped.mark, stamped.generation)),
            Some((Mark::Retarget { waited_ms: 137 }, g))
        );
    }

    #[test]
    fn a_merge_into_a_new_generation_marks_landed_again() {
        let now = Instant::now();
        let mut flights = Journalled::default();
        flights.drive(Event::Begin(pan()), now);
        flights.drive(Event::FrameZero(flights.coordinator().generation()), now);
        let end = now + PLAYS;
        flights.drive(Event::Tick { settled: false, landed: true }, end);
        let first = flights.coordinator().generation();
        flights.drive(
            Event::Merge { stage: pan(), moved: false },
            end + Duration::from_millis(5),
        );
        let merged = flights.coordinator().generation();
        assert_ne!(first, merged);
        flights.drive(
            Event::Tick { settled: false, landed: true },
            end + Duration::from_millis(16),
        );
        let landed: Vec<Generation> = held(&flights)
            .iter()
            .filter(|stamped| stamped.mark == Mark::Landed)
            .map(|stamped| stamped.generation)
            .collect();
        assert_eq!(landed, [first, merged]);
    }

    #[test]
    fn a_lift_carries_the_tick_that_lifted_it() {
        let now = Instant::now();
        let lift = |settled, landed, at| {
            let mut flights = Journalled::default();
            flights.drive(Event::Begin(pan()), now);
            flights.drive(Event::FrameZero(flights.coordinator().generation()), now);
            flights.drive(Event::Tick { settled, landed }, at);
            held(&flights)
                .into_iter()
                .find(|stamped| matches!(stamped.mark, Mark::Lift { .. }))
                .map(|stamped| stamped.mark)
        };
        assert_eq!(
            lift(true, true, now + PLAYS),
            Some(Mark::Lift { at_rest: true, landed: true })
        );
        assert_eq!(
            lift(true, false, now + PLAYS + LIFT_GRACE),
            Some(Mark::Lift { at_rest: true, landed: false }),
            "overdue"
        );
        assert_eq!(
            lift(false, false, now + PLAYS + LIFT_GRACE),
            Some(Mark::Lift { at_rest: false, landed: false })
        );
    }

    #[test]
    fn a_delivery_marks_when_it_was_sent_and_each_picture_when_its_capture_started() {
        let now = Instant::now();
        let mut flights = Journalled::default();
        flights.drive(Event::Begin(pan()), now);
        flights.drive(Event::FrameZero(flights.coordinator().generation()), now);
        let first = flights.coordinator().generation();
        flights.drive(Event::Tick { settled: false, landed: false }, now + PLAYS);
        flights.drive(Event::Merge { stage: pan(), moved: false }, now + PLAYS);
        let merged = flights.coordinator().generation();
        let heard = now + PLAYS + Duration::from_millis(40);
        let frame = |x| r(x, 32.0, 864.0, 1085.0);
        let pictures = [
            (w(1), frame(0.0), heard - Duration::from_micros(37_500)),
            (w(2), frame(864.0), heard - Duration::from_millis(37)),
        ];
        let before = held(&flights).len();
        flights.delivery(
            heard,
            first,
            Why::Landed,
            heard - Duration::from_micros(4_100),
            pictures.into_iter(),
        );
        let marks = &held(&flights)[before..];
        assert_eq!(marks.iter().map(|stamped| stamped.mark).collect::<Vec<_>>(), [
            Mark::Delivery {
                of: first,
                why: Why::Landed,
                pictures: 2,
                sent_before_us: 4_100
            },
            Mark::Delivered {
                window: w(1),
                frame: frame(0.0),
                taken_before_us: 37_500
            },
            Mark::Delivered {
                window: w(2),
                frame: frame(864.0),
                taken_before_us: 37_000
            },
        ]);
        assert_ne!(first, merged);
        assert!(
            marks.iter().all(|stamped| stamped.at == heard && stamped.generation == merged),
            "stamped when the engine heard it, in the generation it was in: stale, since `of` is the one before"
        );
    }

    #[test]
    fn an_empty_frame_zero_delivery_is_one_mark() {
        let now = Instant::now();
        let mut flights = Journalled::default();
        flights.drive(Event::Begin(pan()), now);
        let g = flights.coordinator().generation();
        let before = held(&flights).len();
        flights.delivery(
            now + Duration::from_millis(30),
            g,
            Why::FrameZero,
            now + Duration::from_millis(28),
            std::iter::empty(),
        );
        let marks = &held(&flights)[before..];
        assert_eq!(marks.iter().map(|stamped| stamped.mark).collect::<Vec<_>>(), [
            Mark::Delivery {
                of: g,
                why: Why::FrameZero,
                pictures: 0,
                sent_before_us: 2_000
            },
        ]);
    }

    #[test]
    fn a_drawn_batch_is_marked_with_its_hop_and_its_commit() {
        let now = Instant::now();
        let mut flights = Journalled::default();
        flights.drive(Event::Begin(pan()), now);
        let g = flights.coordinator().generation();
        let sent = now + Duration::from_micros(200);
        let at = |us| sent + Duration::from_micros(us);
        let before = held(&flights).len();
        let played = Played {
            turn: Some(OverlayTurn::Shown),
            rest: RestPlacement::Unplaced,
            crops: Crops {
                fills: 2,
                ground: Duration::from_micros(7_910),
            },
        };
        flights.drawn(at(2_000), 4, played, (sent, at(1_300), at(1_700)));
        let mark = Mark::Drawn {
            seq: 4,
            shown: Some(true),
            places: false,
            crops: 2,
            ground_us: 7_910,
            queued_us: 1_300,
            applied_us: 400,
            flushed_before_us: 300,
        };
        assert_eq!(
            held(&flights)[before..],
            [Stamped {
                at: at(2_000),
                generation: g,
                mark
            }],
            "stamped when the engine heard it"
        );
    }

    /// transitions.jsonl and journal.py read a drain's turn and placement as the booleans `shown`
    /// (`null`: neither) and `places`.
    #[test]
    fn a_drawn_mark_writes_the_turn_and_the_placement_as_booleans() {
        use OverlayTurn::{Lifted, Shown};
        use RestPlacement::{Placed, Unplaced};
        let table = [
            (None, Unplaced, serde_json::Value::Null, false),
            (None, Placed, serde_json::Value::Null, true),
            (Some(Shown), Unplaced, true.into(), false),
            (Some(Shown), Placed, true.into(), true),
            (Some(Lifted), Unplaced, false.into(), false),
            (Some(Lifted), Placed, false.into(), true),
        ];
        for (turn, rest, shown, places) in table {
            let now = Instant::now();
            let mut flights = Journalled::default();
            let before = held(&flights).len();
            let played = Played {
                turn,
                rest,
                crops: Crops::default(),
            };
            flights.drawn(now, 1, played, (now, now, now));
            let json = serde_json::to_value(held(&flights)[before].mark).unwrap();
            assert_eq!(json["shown"], shown, "{turn:?} {rest:?}");
            assert_eq!(json["places"], places, "{turn:?} {rest:?}");
        }
    }

    #[test]
    fn a_main_turn_is_marked_in_the_burst_it_falls_in() {
        let now = Instant::now();
        let ms = Duration::from_millis;
        let mut flights = Journalled::default();
        flights.set_writing(true);
        flights.drive(Event::Begin(pan()), now);
        let g = flights.coordinator().generation();
        flights.drive(Event::FrameZero(g), now);
        let began = now + ms(20);
        let actors = [("renderer", Duration::from_micros(2_500)), ("rest", ms(1))];
        let changes = [
            ("overlay.show", began - ms(12)),
            ("rest.frame", began + Duration::from_micros(300)),
        ];
        flights.main_turn(began, ms(262), &actors, Duration::from_micros(258_500), &changes);
        flights.drive(Event::Tick { settled: true, landed: true }, now + PLAYS);
        let burst = flights
            .drive(Event::Quiet(g), now + PLAYS + SETTLE_BEFORE_CAPTURES)
            .1
            .expect("settled");
        let at = burst
            .marks
            .iter()
            .position(|stamped| matches!(stamped.mark, Mark::MainTurn { .. }))
            .expect("marked");
        assert_eq!(
            burst.marks[at..at + 5],
            [
                Mark::MainTurn {
                    busy_us: 262_000,
                    outside_us: 258_500,
                    actors: 2,
                    changes: 2
                },
                Mark::TurnActor { actor: "renderer", us: 2_500 },
                Mark::TurnActor { actor: "rest", us: 1_000 },
                Mark::TurnChange {
                    what: "overlay.show",
                    before_us: 12_000
                },
                Mark::TurnChange {
                    what: "rest.frame",
                    before_us: -300
                },
            ]
            .map(|mark| Stamped { at: began, generation: g, mark })
        );

        let rest = now + ms(900);
        flights.main_turn(rest, ms(9), &actors, ms(6), &[]);
        let next = rest + ms(100);
        assert!(flights.drive(Event::Begin(pan()), next).1.is_none());
        let g = flights.coordinator().generation();
        flights.drive(Event::FrameZero(g), next);
        flights.drive(Event::Tick { settled: true, landed: true }, next + PLAYS);
        let burst = flights
            .drive(Event::Quiet(g), next + PLAYS + SETTLE_BEFORE_CAPTURES)
            .1
            .expect("settled");
        let turn = burst
            .marks
            .iter()
            .position(|stamped| matches!(stamped.mark, Mark::MainTurn { .. }));
        let begin = burst
            .marks
            .iter()
            .position(|stamped| matches!(stamped.mark, Mark::Begin { .. }));
        assert!(
            turn < begin && turn.is_some(),
            "a turn at rest goes with the next burst, ahead of its flight"
        );
        assert_eq!(burst.marks[turn.unwrap()].at, rest);
    }

    #[test]
    fn a_press_is_stamped_when_the_reactor_took_it() {
        let now = Instant::now();
        let mut journal = Journal::default();
        journal.push(now, Generation(1), extend(5));
        let cause = pressed(now, Duration::from_millis(4));
        journal.press(cause, Generation(2));
        let press = *journal.ring.back().unwrap();
        let mark = Mark::Press { what: "move_focus" };
        assert_eq!(press, Stamped {
            at: cause.at,
            generation: Generation(2),
            mark
        });
        assert!(
            press.at < journal.ring[0].at,
            "pushed after a mark it came before: `t` is unsorted"
        );
    }

    #[test]
    fn the_ring_keeps_the_last_8192_and_counts_what_it_overwrote_unwritten() {
        let now = Instant::now();
        let mut journal = Journal::default();
        for ms in 0..RING as u32 + 10 {
            journal.push(now, Generation(1), extend(ms));
        }
        assert_eq!(journal.ring.len(), RING);
        let burst = journal.unwritten().unwrap();
        assert_eq!(burst.lost, 10);
        assert_eq!(burst.marks.len(), RING);
        assert_eq!(burst.marks[0].mark, extend(10), "the oldest held");
        assert_eq!(burst.marks[RING - 1].mark, extend(RING as u32 + 9));
    }

    #[test]
    fn a_burst_is_every_mark_since_the_last_write() {
        let now = Instant::now();
        let mut journal = Journal::default();
        assert_eq!(journal.unwritten(), None, "nothing to write");
        for ms in 0..3 {
            journal.push(now, Generation(1), extend(ms));
        }
        assert_eq!(journal.unwritten().unwrap().marks.len(), 3);
        journal.push(now, Generation(2), extend(3));
        journal.push(now, Generation(2), extend(4));
        let burst = journal.unwritten().unwrap();
        assert_eq!(
            burst.marks.iter().map(|stamped| stamped.mark).collect::<Vec<_>>(),
            [extend(3), extend(4)]
        );
        assert_eq!(burst.lost, 0);
        assert_eq!(journal.unwritten(), None);
    }

    #[test]
    fn nothing_is_written_off_so_turning_on_writes_the_ring() {
        let now = Instant::now();
        let mut journal = Journal::default();
        for ms in 0..RING as u32 + 5 {
            journal.push(now, Generation(1), extend(ms));
        }
        journal.start_writing();
        let burst = journal.unwritten().unwrap();
        assert_eq!(burst.marks.len(), RING, "every mark the ring still holds");
        assert_eq!(
            burst.lost, 0,
            "what it overwrote while off was never going to be written"
        );
        assert_eq!(burst.marks[0].mark, extend(5));
    }

    #[test]
    fn the_anchor_converts_either_side_of_it() {
        let base = Instant::now();
        let anchor = Anchor {
            at: base + Duration::from_secs(1),
            uptime_ns: 5_000_000_000,
        };
        assert_eq!(anchor.ns(anchor.at), 5_000_000_000);
        assert_eq!(anchor.ns(anchor.at + Duration::from_millis(3)), 5_003_000_000);
        assert_eq!(anchor.ns(anchor.at - Duration::from_micros(2)), 4_999_998_000);
        assert_eq!(
            Anchor { at: anchor.at, uptime_ns: 5 }.ns(base),
            0,
            "never before the clock's start"
        );
    }

    #[test]
    fn a_line_is_one_object_with_t_g_and_mark() {
        let at = Instant::now();
        let anchor = Anchor {
            at,
            uptime_ns: 81_234_567_890_123,
        };
        let stamped = |mark| Stamped {
            at: at + Duration::from_micros(5),
            generation: Generation(42),
            mark,
        };
        let frame = OverlayRect::new(Point::new(0.0, 32.0), Size::new(864.0, 1085.5));
        assert_eq!(
            line(&anchor, &stamped(Mark::Snap { window: w(3104), frame })),
            r#"{"t":81234567895123,"g":42,"mark":"snap","window":[1,3104],"frame":[0.0,32.0,864.0,1085.5]}"#
        );
        assert_eq!(
            line(&anchor, &stamped(Mark::Hold)),
            r#"{"t":81234567895123,"g":42,"mark":"hold"}"#
        );
        assert_eq!(
            line(&anchor, &stamped(Mark::Press { what: "focus_window" })),
            r#"{"t":81234567895123,"g":42,"mark":"press","what":"focus_window"}"#
        );
        assert_eq!(
            line(&anchor, &stamped(Mark::Focus { window: w(2), focused: true })),
            r#"{"t":81234567895123,"g":42,"mark":"focus","window":[1,2],"focused":true}"#
        );
        assert_eq!(
            line(
                &anchor,
                &stamped(Mark::Merge {
                    kind: Kind::LayoutPass,
                    moved: false,
                    tiles: 2,
                    awaiting: 1,
                    repainting: 0,
                    focus: Some(w(7)),
                    focused_face: None,
                    duration_ms: 350,
                })
            ),
            concat!(
                r#"{"t":81234567895123,"g":42,"mark":"merge","kind":"layout_pass","moved":false,"tiles":2,"#,
                r#""awaiting":1,"repainting":0,"focus":[1,7],"focused_look":null,"duration_ms":350}"#
            )
        );
        assert_eq!(
            line(&anchor, &stamped(Mark::Quiet { of: Generation(41) })),
            r#"{"t":81234567895123,"g":42,"mark":"quiet","of":41}"#
        );
        assert_eq!(
            line(
                &anchor,
                &stamped(Mark::Delivery {
                    of: Generation(42),
                    why: Why::FrameZero,
                    pictures: 1,
                    sent_before_us: 4_100,
                })
            ),
            r#"{"t":81234567895123,"g":42,"mark":"delivery","of":42,"why":"frame_zero","pictures":1,"sent_before_us":4100}"#
        );
        let taken_at = r(0.0, 32.0, 864.0, 1085.0);
        assert_eq!(
            line(
                &anchor,
                &stamped(Mark::Delivered {
                    window: w(3104),
                    frame: taken_at,
                    taken_before_us: 31_200
                })
            ),
            r#"{"t":81234567895123,"g":42,"mark":"delivered","window":[1,3104],"frame":[0.0,32.0,864.0,1085.0],"taken_before_us":31200}"#
        );
        let whys = [
            Why::FrameZero,
            Why::Landed,
            Why::FocusChanged,
            Why::Background,
            Why::Final,
        ];
        assert_eq!(whys.map(|why| serde_json::to_value(why).unwrap()), [
            "frame_zero",
            "landed",
            "focus_changed",
            "background",
            "final"
        ]);
        let wearing = [
            Wearing::Compose,
            Wearing::FrameZero,
            Wearing::FocusedFace,
            Wearing::Swap,
            Wearing::Corner,
        ];
        assert_eq!(wearing.map(|at| serde_json::to_value(at).unwrap()), [
            "compose",
            "frame_zero",
            "focused_look",
            "swap",
            "corner"
        ]);
        assert_eq!(
            line(
                &anchor,
                &stamped(Mark::Hide {
                    measured: true,
                    waited_us: 31_400
                })
            ),
            r#"{"t":81234567895123,"g":42,"mark":"hide","measured":true,"waited_us":31400}"#
        );
        let handover = Mark::Handover {
            of: Generation(41),
            window: w(3104),
            interior: 0.26,
            interior_over: 0.0012,
            edge_in: 0.37,
            edge_out: 1.56,
            band: None,
        };
        assert_eq!(
            line(&anchor, &stamped(handover)),
            concat!(
                r#"{"t":81234567895123,"g":42,"mark":"handover","of":41,"window":[1,3104],"interior":0.26,"#,
                r#""interior_over":0.0012,"edge_in":0.37,"edge_out":1.56}"#
            ),
            "no band, no column"
        );
        let banded = Mark::Handover {
            of: Generation(41),
            window: w(3104),
            interior: 0.26,
            interior_over: 0.0012,
            edge_in: 0.37,
            edge_out: 1.56,
            band: Some(0.5),
        };
        assert!(line(&anchor, &stamped(banded)).ends_with(r#""edge_out":1.56,"band":0.5}"#));
        let burst = Burst { marks: Vec::new(), lost: 3 };
        assert_eq!(
            header(&anchor, 1_791_043_200_123, &burst),
            r#"{"t":81234567890123,"mark":"burst","journal":1,"unix_ms":1791043200123,"lost":3}"#
        );
    }

    #[test]
    fn a_flight_journals_hold_show_write_watch_lift_and_report_settled_in_order() {
        let mut flights = Journalled::default();
        fly(&mut flights, Instant::now());
        let marks = held(&flights);
        let order = ["hold", "show", "write", "watch", "lift", "report_settled"];
        let found: Vec<String> = names(&marks)
            .into_iter()
            .filter(|name| order.contains(&name.as_str()))
            .collect();
        assert_eq!(found, order);
        assert!(
            marks.iter().all(|stamped| stamped.generation == Generation(1)),
            "one generation: {marks:?}"
        );
        assert_eq!(
            names(&marks[..5]),
            ["press", "begin", "hold", "ask_frame_zero", "report_up"],
            "the press, the event, then its commands"
        );
    }

    /// The overview's lifecycle is journalled as a flight's is, under a generation of its own: the
    /// press that opened it, its open, each command it gives that a flight's mark names, its own
    /// steps, the lift with what the tick that lifted it read, and the burst once it settles.
    #[test]
    fn the_overview_journals_each_step_under_its_own_generation() {
        use super::overview::{Command as Gave, Event as Step};
        let named = |marks: &[Stamped]| -> Vec<String> {
            marks
                .iter()
                .map(|stamped| {
                    let value = serde_json::to_value(stamped.mark).unwrap();
                    match value.get("step") {
                        Some(step) => format!("overview:{}", step.as_str().unwrap()),
                        None => name(&stamped.mark),
                    }
                })
                .collect()
        };
        let now = Instant::now();
        let arc = Duration::from_millis(257);
        let mut flights = Journalled::default();
        flights.set_writing(true);
        let generation = flights.reserve();
        flights.pressed(pressed(now, Duration::from_millis(1)));
        let (gave, _) = flights.drive_overview(
            Step::Open {
                generation,
                arc,
                flying: false,
                rests: true,
            },
            now,
        );
        assert!(gave.contains(&Gave::Clock(true)));
        flights.drive_overview(Step::FrameZero(generation), now);
        flights.drive_overview(Step::Scene, now);
        flights.drive_overview(Step::Tick { settled: false, landed: false }, now + arc);
        flights.drive_overview(Step::Close, now + arc);
        let landed = Step::Tick { settled: true, landed: true };
        flights.drive_overview(landed, now + arc * 2);
        flights.drive_overview(landed, now + arc * 3);
        let (_, burst) = flights.drive_overview(Step::Quiet(generation), now + arc * 4);
        let marks = held(&flights);
        assert_eq!(named(&marks), [
            "press",
            "overview:open",
            "hold",
            "overview:ask_frame_zero",
            "report_up",
            "show",
            "overview:scene",
            "overview:played",
            "release",
            "overview:close",
            "hold",
            "overview:exit",
            "place_rest",
            "lift",
            "settle",
            "overview:replay",
            "quiet",
            "release",
            "report_settled",
        ]);
        assert!(marks.iter().all(|stamped| stamped.generation == generation));
        assert!(matches!(
            marks
                .iter()
                .find(|stamped| matches!(stamped.mark, Mark::Lift { .. }))
                .unwrap()
                .mark,
            Mark::Lift { at_rest: true, landed: true }
        ));
        assert_eq!(
            burst.map(|burst| burst.marks.len()),
            Some(marks.len()),
            "written once settled"
        );
    }

    #[test]
    fn a_press_is_marked_once_ahead_of_its_requests_first_drive() {
        let now = Instant::now();
        let mut flights = Journalled::default();
        let cause = pressed(now, Duration::from_millis(2));
        flights.pressed(cause);
        flights.drive(Event::Begin(pan()), now);
        flights.drive(Event::FrameZero(flights.coordinator().generation()), now);
        flights.answered();
        let first = flights.coordinator().generation();
        let marks = held(&flights);
        assert_eq!(names(&marks[..2]), ["press", "begin"]);
        let mark = Mark::Press { what: "move_focus" };
        assert_eq!(presses(&marks), [Stamped {
            at: cause.at,
            generation: first,
            mark
        }]);

        flights.drive(Event::Tick { settled: false, landed: false }, now + PLAYS);
        let before = held(&flights).len();
        flights.pressed(pressed(now + PLAYS, Duration::from_millis(1)));
        flights.drive(Event::Merge { stage: pan(), moved: false }, now + PLAYS);
        flights.drive(Event::Extend(Duration::from_millis(5)), now + PLAYS);
        flights.answered();
        let merged = flights.coordinator().generation();
        assert_ne!(first, merged);
        let marks = &held(&flights)[before..];
        assert_eq!(names(&marks[..2]), ["press", "merge"]);
        assert_eq!(
            presses(marks).len(),
            1,
            "once, by the first of the request's drives"
        );
        assert_eq!(marks[0].generation, merged, "in the generation its drive left");
    }

    #[test]
    fn a_request_answered_with_no_drive_marks_no_press() {
        let now = Instant::now();
        let mut flights = Journalled::default();
        flights.pressed(pressed(now, Duration::from_millis(1)));
        flights.answered();
        flights.drive(Event::FacesTurned(vec![(w(1), r(0.0, 0.0, 200.0, 200.0))]), now);
        flights.drive(Event::Begin(pan()), now);
        let marks = held(&flights);
        assert!(presses(&marks).is_empty(), "{marks:?}");
        assert_eq!(names(&marks[..1]), ["looks_turned"]);
    }

    /// Drives `event` at `at` through a bare coordinator and through each journalled one: every drive
    /// gives what the bare one says, in its order.
    fn drives_alike(
        bare: &mut Coordinator,
        journalled: &mut [Journalled],
        event: Event,
        at: Instant,
    ) -> usize {
        let said = bare.on(event.clone(), at);
        for flights in journalled.iter_mut() {
            assert_eq!(flights.drive(event.clone(), at).0, said, "{event:?}");
        }
        said.len()
    }

    /// Writing the journal or not changes nothing the coordinator says: a drive gives the commands a
    /// bare coordinator gives for the same events at the same instants, never filtered, reordered or
    /// held, with the journal never written, written, and turned on then off.
    #[test]
    fn a_drive_gives_what_the_coordinator_says_whether_the_journal_is_written_or_not() {
        let now = Instant::now();
        let mut written = Journalled::default();
        written.set_writing(true);
        let mut toggled = Journalled::default();
        toggled.set_writing(true);
        toggled.set_writing(false);
        let mut journalled = [Journalled::default(), written, toggled];
        let mut bare = Coordinator::default();
        let mut said = drives_alike(&mut bare, &mut journalled, Event::Begin(pan()), now);
        let at = |ms| now + Duration::from_millis(ms);
        said += drives_alike(
            &mut bare,
            &mut journalled,
            Event::Merge { stage: pan(), moved: false },
            at(5),
        );
        let g = bare.generation();
        said += drives_alike(&mut bare, &mut journalled, Event::FrameZero(g), at(30));
        said += drives_alike(
            &mut bare,
            &mut journalled,
            Event::Tick { settled: false, landed: false },
            at(100),
        );
        let frame = r(0.0, 0.0, 200.0, 200.0);
        said += drives_alike(
            &mut bare,
            &mut journalled,
            Event::FacesTurned(vec![(w(2), frame)]),
            at(120),
        );
        said += drives_alike(
            &mut bare,
            &mut journalled,
            Event::Tick { settled: true, landed: true },
            now + PLAYS,
        );
        said += drives_alike(
            &mut bare,
            &mut journalled,
            Event::Quiet(g),
            now + PLAYS + SETTLE_BEFORE_CAPTURES,
        );
        assert!(said > 10, "a whole flight said something: {said}");
        assert!(journalled.iter().all(|flights| flights.coordinator().generation() == g));
    }

    #[test]
    fn a_settled_burst_is_given_once_while_writing() {
        let mut flights = Journalled::default();
        assert_eq!(flights.set_writing(true), None, "an empty ring");
        let bursts = fly(&mut flights, Instant::now());
        assert!(
            bursts[..3].iter().all(Option::is_none),
            "nothing is written while a burst is up"
        );
        let burst = bursts[3].as_ref().expect("given at its settle");
        assert_eq!(
            burst.marks,
            held(&flights),
            "every mark of it, its own settle included"
        );
        assert_eq!(name(&burst.marks.last().unwrap().mark), "report_settled");
        assert_eq!(
            flights.drive(Event::Quiet(Generation(1)), Instant::now()).1,
            None,
            "once"
        );
    }

    #[test]
    fn no_burst_is_given_while_not_writing() {
        let mut flights = Journalled::default();
        assert!(fly(&mut flights, Instant::now()).iter().all(Option::is_none));
        assert!(!held(&flights).is_empty(), "the ring still holds it");
        flights.set_writing(true);
        assert_eq!(flights.set_writing(false), None);
        assert!(fly(&mut flights, Instant::now()).iter().all(Option::is_none));
    }

    #[test]
    fn turning_writing_on_at_rest_gives_the_ring_at_once() {
        let mut flights = Journalled::default();
        fly(&mut flights, Instant::now());
        let burst = flights.set_writing(true).expect("the ring at once");
        assert_eq!(burst.marks, held(&flights));
        assert_eq!(burst.lost, 0);
        assert_eq!(flights.set_writing(true), None, "already on");
    }

    #[test]
    fn turning_writing_on_in_flight_gives_it_at_the_next_settle() {
        let now = Instant::now();
        let mut flights = Journalled::default();
        flights.drive(Event::Begin(pan()), now);
        assert_eq!(flights.set_writing(true), None, "a burst is up");
        let g = flights.coordinator().generation();
        flights.drive(Event::FrameZero(g), now);
        flights.drive(Event::Tick { settled: true, landed: true }, now + PLAYS);
        let (_, burst) = flights.drive(Event::Quiet(g), now + PLAYS + SETTLE_BEFORE_CAPTURES);
        let burst = burst.expect("given at the settle");
        assert_eq!(
            names(&burst.marks[..1]),
            ["begin"],
            "from before writing turned on"
        );
        assert_eq!(burst.marks, held(&flights));
    }

    #[test]
    fn hides_on_its_overlay_picture_a_request_or_patience_only() {
        let since = Instant::now();
        let waiting = LiftWait {
            generation: Generation(4),
            since,
        };
        let at = |ms| since + Duration::from_millis(ms);
        let taken = |generation, taken| Heard::OverlayTaken {
            generation: Generation(generation),
            taken,
        };
        assert_eq!(hides(waiting, taken(4, true), at(31)), Some(Hide::Measured));
        assert_eq!(
            hides(waiting, taken(4, false), at(31)),
            Some(Hide::Unmeasured),
            "no picture to compare"
        );
        assert_eq!(hides(waiting, taken(3, true), at(31)), None, "another lift's");
        assert_eq!(
            hides(waiting, Heard::Request, at(5)),
            Some(Hide::Unmeasured),
            "a press never waits"
        );
        assert_eq!(
            hides(waiting, Heard::Moved, at(5)),
            Some(Hide::Unmeasured),
            "never moved with its tiles on"
        );
        assert_eq!(
            hides(waiting, Heard::Patience, at(59)),
            None,
            "early, or another wait's timer"
        );
        assert_eq!(
            hides(waiting, Heard::Patience, since + OVERLAY_TAKE_PATIENCE),
            Some(Hide::Unmeasured)
        );
    }

    /// A lift is held only while the handover is measured, and only with a window ending wholly on
    /// screen to compare.
    #[test]
    fn a_lift_is_held_only_while_measuring_with_a_window_to_compare() {
        let ends = [(w(1), r(0.0, 0.0, 200.0, 200.0))];
        assert!(measures(true, &ends));
        assert!(!measures(false, &ends), "not measuring");
        assert!(!measures(true, &[]), "nothing ending on screen");
    }

    /// The pan, journalled, to its lift, which the engine holds: the generation that lifted, and when.
    fn held_lift(flights: &mut Journalled, now: Instant) -> (Generation, Instant) {
        flights.drive(Event::Begin(pan()), now);
        let g = flights.coordinator().generation();
        flights.drive(Event::FrameZero(g), now);
        let lifted = now + PLAYS;
        let (commands, _) = flights.drive(Event::Tick { settled: true, landed: true }, lifted);
        assert!(commands.contains(&Command::Lift { generation: g }));
        flights.lifted(g, lifted);
        (g, lifted)
    }

    fn diff(interior: f32) -> HandoverDiff {
        HandoverDiff {
            interior,
            interior_over: 0.0,
            edge_in: 0.4,
            edge_out: 1.5,
            band: None,
        }
    }

    fn found(marks: &[Stamped], name: &str) -> Vec<Mark> {
        marks
            .iter()
            .filter(|stamped| self::name(&stamped.mark) == name)
            .map(|stamped| stamped.mark)
            .collect()
    }

    #[test]
    fn a_measured_lift_waits_for_the_overlay_picture_and_journals_the_handover() {
        let now = Instant::now();
        let mut flights = Journalled::default();
        flights.set_writing(true);
        let (g, lifted) = held_lift(&mut flights, now);
        assert!(
            !flights.heard(Heard::Patience, lifted + Duration::from_millis(20)),
            "held"
        );
        let taken = lifted + Duration::from_millis(31);
        assert!(
            !flights.heard(
                Heard::OverlayTaken {
                    generation: Generation(g.0 + 1),
                    taken: true
                },
                taken
            ),
            "another lift's"
        );
        assert!(flights.heard(Heard::OverlayTaken { generation: g, taken: true }, taken));
        assert!(!flights.heard(Heard::Request, taken), "hidden once");
        flights.handover(taken + Duration::from_millis(70), g, [
            (w(2), diff(0.3)),
            (w(1), diff(0.8)),
        ]);
        let (_, burst) = flights.drive(Event::Quiet(g), lifted + SETTLE_BEFORE_CAPTURES);
        let marks = burst.expect("given at the settle").marks;
        assert_eq!(found(&marks, "hide"), [Mark::Hide {
            measured: true,
            waited_us: 31_000
        }]);
        let handover = |window, interior| Mark::Handover {
            of: g,
            window,
            interior,
            interior_over: 0.0,
            edge_in: 0.4,
            edge_out: 1.5,
            band: None,
        };
        assert_eq!(found(&marks, "handover"), [
            handover(w(2), 0.3),
            handover(w(1), 0.8)
        ]);
        let at = |name: &str| names(&marks).iter().position(|found| found == name).unwrap();
        assert!(
            at("lift") < at("hide") && at("hide") < at("handover"),
            "{:?}",
            names(&marks)
        );
        assert!(at("handover") < at("report_settled"), "in its own burst");
    }

    /// A lift that hid on the patience may be gone from a picture taken later, so its handover, if one
    /// comes, is not journalled.
    #[test]
    fn a_lift_hidden_on_the_patience_journals_no_handover() {
        let now = Instant::now();
        let mut flights = Journalled::default();
        let (g, lifted) = held_lift(&mut flights, now);
        assert!(flights.heard(Heard::Patience, lifted + OVERLAY_TAKE_PATIENCE));
        assert!(!flights.heard(
            Heard::OverlayTaken { generation: g, taken: true },
            lifted + OVERLAY_TAKE_PATIENCE
        ));
        flights.handover(lifted + Duration::from_millis(120), g, [(w(2), diff(0.3))]);
        let marks = held(&flights);
        assert_eq!(found(&marks, "hide"), [Mark::Hide {
            measured: false,
            waited_us: 60_000
        }]);
        assert!(found(&marks, "handover").is_empty());
    }

    /// A press never waits for the overlay's picture: a request hides the lift at once, unmeasured.
    #[test]
    fn a_request_while_a_lift_waits_hides_it_at_once_unmeasured() {
        let now = Instant::now();
        let mut flights = Journalled::default();
        let (g, lifted) = held_lift(&mut flights, now);
        let pressed = lifted + Duration::from_millis(5);
        assert!(flights.heard(Heard::Request, pressed));
        flights.handover(pressed + Duration::from_millis(80), g, [(w(2), diff(0.3))]);
        let marks = held(&flights);
        assert_eq!(found(&marks, "hide"), [Mark::Hide {
            measured: false,
            waited_us: 5_000
        }]);
        assert!(found(&marks, "handover").is_empty());
        assert!(!flights.heard(Heard::Request, pressed), "nothing waits any more");
    }

    /// The overlay moving to another display hides a held lift first, unmeasured: it would carry the
    /// finished flight's tiles there.
    #[test]
    fn the_overlay_moving_hides_a_held_lift_first_unmeasured() {
        let now = Instant::now();
        let mut flights = Journalled::default();
        let (g, lifted) = held_lift(&mut flights, now);
        let moved = lifted + Duration::from_millis(12);
        assert!(flights.heard(Heard::Moved, moved));
        flights.handover(moved + Duration::from_millis(80), g, [(w(2), diff(0.3))]);
        let marks = held(&flights);
        assert_eq!(found(&marks, "hide"), [Mark::Hide {
            measured: false,
            waited_us: 12_000
        }]);
        assert!(found(&marks, "handover").is_empty());
    }

    /// Only the handover of the lift that hid on its picture is journalled, once.
    #[test]
    fn a_handover_for_another_generation_is_dropped() {
        let now = Instant::now();
        let mut flights = Journalled::default();
        let (g, lifted) = held_lift(&mut flights, now);
        let taken = lifted + Duration::from_millis(31);
        assert!(flights.heard(Heard::OverlayTaken { generation: g, taken: true }, taken));
        flights.handover(taken + Duration::from_millis(70), Generation(g.0 - 1), [(
            w(2),
            diff(0.3),
        )]);
        assert!(found(&held(&flights), "handover").is_empty(), "another lift's");
        flights.handover(taken + Duration::from_millis(71), g, [(w(2), diff(0.3))]);
        flights.handover(taken + Duration::from_millis(72), g, [(w(2), diff(0.4))]);
        assert_eq!(found(&held(&flights), "handover").len(), 1, "once");
    }

    /// A drain that turned the overlay as `turn` says and placed what rests as `rest`, drawing no
    /// tile on the crop grid.
    fn played(turn: Option<OverlayTurn>, rest: RestPlacement) -> Played {
        Played {
            turn,
            rest,
            crops: Crops::default(),
        }
    }

    /// A script of drives, deliveries and presses `us` microseconds from `base`, collecting the bursts
    /// they give.
    struct Script {
        flights: Journalled,
        base: Instant,
        bursts: Vec<(Instant, Burst)>,
    }

    impl Script {
        fn at(&self, us: i64) -> Instant {
            let span = Duration::from_micros(us.unsigned_abs());
            if us < 0 {
                self.base - span
            } else {
                self.base + span
            }
        }

        fn press(&mut self, us: i64, what: &'static str) {
            self.flights.pressed(Cause { at: self.at(us), what });
        }

        fn drive(&mut self, us: i64, event: Event) {
            let now = self.at(us);
            if let Some(burst) = self.flights.drive(event, now).1 {
                self.bursts.push((now, burst));
            }
        }

        fn deliver(
            &mut self,
            us: i64,
            of: Generation,
            why: Why,
            sent: i64,
            pictures: &[(WindowId, ScreenRect, i64)],
        ) {
            let taken =
                pictures.iter().map(|&(window, frame, taken)| (window, frame, self.at(taken)));
            let taken: Vec<_> = taken.collect();
            self.flights.delivery(self.at(us), of, why, self.at(sent), taken.into_iter());
        }

        /// The engine holds `generation`'s lift, which its drive at `us` gave.
        fn lifted(&mut self, us: i64, generation: Generation) {
            self.flights.lifted(generation, self.at(us));
        }

        fn heard(&mut self, us: i64, heard: Heard) {
            assert!(self.flights.heard(heard, self.at(us)), "the held lift hides");
        }

        fn handover(&mut self, us: i64, of: Generation, windows: &[(WindowId, HandoverDiff)]) {
            self.flights.handover(self.at(us), of, windows.iter().copied());
        }

        /// The renderer's commit of the drain up to `seq`, heard at `us`, which `played`.
        fn drawn(
            &mut self,
            us: i64,
            seq: u64,
            played: Played,
            (sent, began, flushed): (i64, i64, i64),
        ) {
            let (sent, began, flushed) = (self.at(sent), self.at(began), self.at(flushed));
            self.flights.drawn(self.at(us), seq, played, (sent, began, flushed));
        }

        /// The picture `window`'s tile took `at` that moment, heard at `us`, its capture started at `taken`.
        fn wears(
            &mut self,
            us: i64,
            window: WindowId,
            at: Wearing,
            route: Route,
            blur: bool,
            taken: i64,
        ) {
            self.flights.wears(self.at(us), window, at, route, blur, self.at(taken));
        }

        /// A turn of the main thread from `us` for `busy`, its actors' polls and its window changes,
        /// each at its `us`.
        fn turn(
            &mut self,
            us: i64,
            busy: u64,
            actors: &[(&'static str, u64)],
            changes: &[(&'static str, i64)],
        ) {
            let actors: Vec<_> = actors
                .iter()
                .map(|&(actor, took)| (actor, Duration::from_micros(took)))
                .collect();
            let changes: Vec<_> = changes.iter().map(|&(what, at)| (what, self.at(at))).collect();
            let polled: u64 = actors.iter().map(|(_, took)| took.as_micros() as u64).sum();
            let outside = Duration::from_micros(busy - polled);
            self.flights.main_turn(
                self.at(us),
                Duration::from_micros(busy),
                &actors,
                outside,
                &changes,
            );
        }

        /// A delivery and the pictures the engine offers the coordinator from it, 0.1ms later.
        fn offer(
            &mut self,
            us: i64,
            of: Generation,
            why: Why,
            sent: i64,
            pictures: &[(WindowId, ScreenRect, i64)],
        ) {
            self.deliver(us, of, why, sent, pictures);
            for &(window, frame, _) in pictures {
                self.drive(us + 100, Event::Picture {
                    generation: of,
                    window,
                    frame,
                    why,
                });
            }
        }
    }

    /// Two bursts as the journal writes them, the handover measured, which
    /// `tools/focuslab/test_journal.py` reads, and the numbers it must find there. A pan with focus
    /// handed over by the target's focused picture, swapped in 15ms after it came as the window losing
    /// focus showed no turn of its own, its lift measured; then one whose frame zero comes
    /// after the patience, a second press merged into it, a stale delivery, a window lost and an
    /// overdue lift, which hid unmeasured. The renderer's commits of each show and each hide are
    /// marked, and of the first press's composed flight before its show; a slow turn of the main thread
    /// just after the first show, and one at rest before the second press.
    #[test]
    fn the_sample_the_tools_read_is_what_the_journal_writes() {
        let mut script = Script {
            flights: Journalled::default(),
            base: Instant::now(),
            bursts: Vec::new(),
        };
        script.flights.set_writing(true);
        let starts = r(3.0, 35.0, 860.0, 1079.0);
        let left = r(866.0, 35.0, 860.0, 1079.0);
        let floating = r(224.0, 95.0, 600.0, 400.0);
        let target = r(3.0, 35.0, 861.0, 1079.0);

        script.press(-2_400, "move_focus");
        script.drive(0, Event::Begin(pan()));
        script.wears(6_000, w(1), Wearing::Compose, Route::Baked, true, -2_100_000);
        script.wears(
            6_000,
            w(2),
            Wearing::Compose,
            Route::ScreenCaptureKit,
            false,
            -900_000,
        );
        script.wears(6_000, w(9), Wearing::Compose, Route::SkyLight, false, -1_600_000);
        script.drive(6_100, Event::Merge { stage: pan(), moved: false });
        script.flights.answered();
        script.drawn(
            7_000,
            2,
            played(None, RestPlacement::Unplaced),
            (6_200, 6_400, 6_800),
        );
        let g = script.flights.coordinator().generation();
        script.deliver(33_200, g, Why::FrameZero, 31_000, &[
            (w(1), starts, 1_300),
            (w(9), floating, 16_500),
        ]);
        script.wears(33_300, w(1), Wearing::FrameZero, Route::Baked, true, 1_300);
        script.wears(33_300, w(9), Wearing::FrameZero, Route::SkyLight, false, 16_500);
        script.drive(33_600, Event::FrameZero(g));
        script.drive(34_500, Event::Extend(Duration::from_millis(349)));
        script.drawn(
            35_500,
            3,
            Played {
                crops: Crops {
                    fills: 1,
                    ground: Duration::from_micros(7_910),
                },
                ..played(Some(OverlayTurn::Shown), RestPlacement::Unplaced)
            },
            (34_600, 34_900, 35_200),
        );
        script.turn(
            36_000,
            11_800,
            &[("renderer", 400), ("rest", 3_100), ("window_notify", 0)],
            &[("overlay.show", 35_100), ("rest.frame", 36_500)],
        );
        script.offer(95_000, g, Why::Landed, 93_100, &[(w(2), target, 88_400)]);
        script.wears(95_100, w(2), Wearing::Swap, Route::Baked, true, 88_400);
        script.offer(112_000, g, Why::Landed, 110_200, &[(w(1), left, 104_000)]);
        script.wears(112_100, w(1), Wearing::Swap, Route::Baked, true, 104_000);
        script.offer(141_000, g, Why::FocusChanged, 139_000, &[(
            w(2),
            target,
            131_500,
        )]);
        script.drive(156_100, Event::Tick { settled: false, landed: false });
        script.offer(160_000, g, Why::Background, 158_400, &[(
            w(9),
            floating,
            152_000,
        )]);
        script.drive(273_600, Event::Tick { settled: false, landed: false });
        script.offer(312_000, g, Why::Final, 309_500, &[(w(1), left, 280_200)]);
        script.drive(370_000, Event::Tick { settled: false, landed: true });
        script.drawn(
            372_000,
            8,
            played(None, RestPlacement::Placed),
            (370_200, 370_400, 371_700),
        );
        script.drive(383_700, Event::Tick { settled: true, landed: true });
        script.lifted(383_700, g);
        script.heard(415_100, Heard::OverlayTaken { generation: g, taken: true });
        script.drawn(
            416_000,
            9,
            played(Some(OverlayTurn::Lifted), RestPlacement::Unplaced),
            (415_200, 415_400, 415_700),
        );
        let diff = |interior, interior_over, edge_in, edge_out, band| HandoverDiff {
            interior,
            interior_over,
            edge_in,
            edge_out,
            band,
        };
        let measured = [
            (w(2), diff(0.26, 0.0012, 0.37, 1.56, None)),
            (w(1), diff(0.79, 0.004, 1.08, 2.26, Some(0.42))),
        ];
        script.handover(480_300, g, &measured);
        script.drive(783_700, Event::Quiet(g));
        script.turn(1_500_000, 9_200, &[("bar", 8_700)], &[("bar.frame", 1_492_000)]);

        let wider = r(866.0, 35.0, 600.0, 1079.0);
        let mut resized = pan();
        resized.tiles[0].end = wider;
        resized.final_frames[0].1 = wider;
        script.press(2_998_200, "move_focus");
        script.drive(3_000_000, Event::Begin(pan()));
        script.wears(3_004_900, w(1), Wearing::Compose, Route::Baked, true, 1_002_000);
        script.wears(
            3_004_900,
            w(2),
            Wearing::FocusedFace,
            Route::Baked,
            true,
            2_104_000,
        );
        script.wears(
            3_004_900,
            w(9),
            Wearing::Compose,
            Route::StandIn,
            false,
            3_004_900,
        );
        script.drive(3_005_000, Event::Merge { stage: pan(), moved: false });
        script.flights.answered();
        let first = script.flights.coordinator().generation();
        script.drive(3_080_000, Event::Tick { settled: false, landed: false });
        script.drive(3_080_900, Event::Extend(Duration::from_millis(349)));
        script.drawn(
            3_082_300,
            12,
            played(Some(OverlayTurn::Shown), RestPlacement::Unplaced),
            (3_081_000, 3_081_400, 3_081_900),
        );
        let late = [(w(1), starts, 3_002_000), (w(9), floating, 3_002_000)];
        script.offer(3_096_000, first, Why::FrameZero, 3_093_500, &late);
        script.wears(3_096_100, w(1), Wearing::Swap, Route::Baked, true, 3_002_000);
        script.drive(3_096_300, Event::FrameZero(first));
        script.press(3_148_700, "cycle_preset_column_width");
        script.drive(3_150_000, Event::Merge { stage: resized, moved: true });
        script.flights.answered();
        let merged = script.flights.coordinator().generation();
        script.offer(3_170_000, first, Why::Landed, 3_168_000, &[(
            w(2),
            target,
            3_141_000,
        )]);
        script.offer(3_200_000, merged, Why::Landed, 3_197_500, &[(
            w(2),
            target,
            3_190_000,
        )]);
        script.drive(3_330_000, Event::Lost {
            generation: merged,
            window: w(1),
        });
        script.drive(3_390_000, Event::Tick { settled: false, landed: false });
        script.drive(3_500_100, Event::Tick { settled: true, landed: false });
        script.drive(3_850_000, Event::Tick { settled: true, landed: false });
        script.lifted(3_850_000, merged);
        script.heard(3_910_000, Heard::Patience);
        script.drawn(
            3_911_000,
            17,
            played(Some(OverlayTurn::Lifted), RestPlacement::Unplaced),
            (3_910_100, 3_910_300, 3_910_700),
        );
        script.drive(4_250_000, Event::Quiet(merged));

        let anchor = |at: Instant| Anchor {
            at,
            uptime_ns: 81_234_560_000_000 + nanos(at - script.base),
        };
        let mut written = String::new();
        for (settled, burst) in &script.bursts {
            let anchor = anchor(*settled + Duration::from_micros(300));
            let unix_ms = 1_791_043_200_000 + u64::from(millis(anchor.at - script.base));
            for text in lines(&anchor, unix_ms, burst) {
                written.push_str(&text);
                written.push('\n');
            }
        }
        assert_eq!(script.bursts.len(), 2, "one burst a flight, each at its quiet");
        assert_eq!(
            written,
            include_str!("../../../tools/focuslab/testdata/journal-sample.jsonl")
        );
    }

    /// Run with `CARGO_TARGET_DIR=target-dev cargo test --release --lib a_push_costs -- --ignored
    /// --nocapture`; the number is in `src/transitions/docs/journal.md`.
    #[test]
    #[ignore]
    fn a_push_costs_a_few_nanoseconds() {
        const PUSHES: u32 = 1_000_000;
        let now = Instant::now();
        let mut journal = Journal::default();
        let started = Instant::now();
        for ms in 0..PUSHES {
            journal.push(now, Generation(u64::from(ms)), std::hint::black_box(extend(ms)));
        }
        let mean = started.elapsed().as_secs_f64() * 1e9 / f64::from(PUSHES);
        std::hint::black_box(&journal);
        println!("a push: {mean:.1}ns, the mean of {PUSHES}");
        assert!(mean < 100.0, "{mean:.1}ns");
    }
}
