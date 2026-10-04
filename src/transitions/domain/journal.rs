//! The transitions journal: the press each request answers, every event the lifecycle takes, every
//! command it gives and every delivery of pictures to it, a mark in a ring kept with the coordinator,
//! and the JSON lines a burst of marks is written as. Pure: the engine drives the lifecycle, tells it
//! each request's press and each delivery through [`Journalled`], and hands each burst it gives to the
//! writer (`crate::transitions::platform::journal`). The marks, the clock and when a burst is given:
//! `src/transitions/docs/journal.md`.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use rini_core::ids::WindowId;
use rini_geometry::space::{OverlayRect, Rect, ScreenRect};
use serde::{Serialize, Serializer};
use serde_with::{SerializeAs, serde_as};

use super::lifecycle::{Command, Coordinator, Event, Generation, Kind, Start};
use super::request::Cause;
use crate::pictures::domain::watch::Why;

/// How many marks the ring holds.
pub const RING: usize = 8192;

/// The line format's version, in every burst's header.
const FORMAT: u32 = 1;

/// One step of a transition: the press a request answers, an event the coordinator took, a command
/// it gave, or a delivery of pictures to it.
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
        focused_look: Option<WindowId>,
        duration_ms: u32,
    },
    Extend {
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
    LooksTurned {
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
    KeepFocusedLook {
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
    HandOverFocus {
        #[serde_as(as = "PidIdx")]
        to: WindowId,
        #[serde_as(as = "Option<PidIdx>")]
        from: Option<WindowId>,
    },
    /// What the tick that lifted it read: either false, `LIFT_GRACE` ran out.
    Lift {
        at_rest: bool,
        landed: bool,
    },
    ReportUp,
    ReportSettled,
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

/// The line ahead of a burst's marks: when it was written, on both clocks, and how many marks it lost.
pub fn header(anchor: &Anchor, unix_ms: u64, burst: &Burst) -> String {
    #[derive(Serialize)]
    struct Header {
        t: u64,
        mark: &'static str,
        journal: u32,
        unix_ms: u64,
        lost: u64,
    }
    json(&Header { t: anchor.uptime_ns, mark: "burst", journal: FORMAT, unix_ms, lost: burst.lost })
}

pub fn line(anchor: &Anchor, stamped: &Stamped) -> String {
    #[derive(Serialize)]
    struct Line<'a> {
        t: u64,
        g: Generation,
        #[serde(flatten)]
        mark: &'a Mark,
    }
    json(&Line { t: anchor.ns(stamped.at), g: stamped.generation, mark: &stamped.mark })
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
        [origin.x as f32, origin.y as f32, size.width as f32, size.height as f32].serialize(serializer)
    }
}

fn nanos(span: Duration) -> u64 {
    u64::try_from(span.as_nanos()).unwrap_or(u64::MAX)
}

fn millis(span: Duration) -> u32 {
    u32::try_from(span.as_millis()).unwrap_or(u32::MAX)
}

fn micros(span: Duration) -> u32 {
    u32::try_from(span.as_micros()).unwrap_or(u32::MAX)
}

fn count(n: usize) -> u16 {
    u16::try_from(n).unwrap_or(u16::MAX)
}

/// An event's marks, read before the coordinator takes it.
enum Pending {
    Nothing,
    One(Mark),
    /// A tick's reading, taken only once the clock has run out: before that the engine reads nothing.
    Tick { settled: bool, landed: bool },
    LooksTurned(Vec<WindowId>),
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
        Journal { ring: VecDeque::with_capacity(RING), first: 0, written: 0, edges: Edges::default() }
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
                awaiting: count(stage.awaiting.len()),
                repainting: count(stage.repainting.len()),
                focus: stage.focus,
                losing_focus: stage.losing_focus,
                key: stage.key,
                duration_ms: millis(stage.duration),
            }),
            Event::Merge { stage, moved } => Pending::One(Mark::Merge {
                kind: stage.kind,
                moved: *moved,
                tiles: count(stage.tiles.len()),
                awaiting: count(stage.awaiting.len()),
                repainting: count(stage.repainting.len()),
                focus: stage.focus,
                focused_look: stage.focused_look,
                duration_ms: millis(stage.duration),
            }),
            Event::FrameZero(_) | Event::Picture { .. } => Pending::Nothing,
            Event::Lost { generation, window } => Pending::One(Mark::Lost { of: *generation, window: *window }),
            Event::Extend(needs) => Pending::One(Mark::Extend { ms: millis(*needs) }),
            Event::Tick { settled, landed } if reads_landing => Pending::Tick { settled: *settled, landed: *landed },
            Event::Tick { .. } => Pending::Nothing,
            Event::Quiet(of) => Pending::One(Mark::Quiet { of: *of }),
            Event::Abandon => Pending::One(Mark::Abandon),
            Event::LooksTurned(windows) => Pending::LooksTurned(windows.iter().map(|(window, _)| *window).collect()),
        }
    }

    /// The event's marks and one per command, stamped `now` and the generation the drive left.
    fn after(&mut self, now: Instant, generation: Generation, pending: Pending, commands: &[Command]) {
        let mut read = (false, false);
        match pending {
            Pending::Nothing => {}
            Pending::One(mark) => self.push(now, generation, mark),
            Pending::Tick { settled, landed } => {
                read = (settled, landed);
                if first_in(&mut self.edges.at_rest, settled, generation) {
                    self.push(now, generation, Mark::AtRest);
                }
                if first_in(&mut self.edges.landed, landed, generation) {
                    self.push(now, generation, Mark::Landed);
                }
            }
            Pending::LooksTurned(windows) => {
                for window in windows {
                    self.push(now, generation, Mark::LooksTurned { window });
                }
            }
        }
        for command in commands {
            self.push(now, generation, Self::mark(command, read));
        }
    }

    /// `(at_rest, landed)`: what the drive's tick read, which a lift carries.
    fn mark(command: &Command, (at_rest, landed): (bool, bool)) -> Mark {
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
            },
            Command::StopWatches => Mark::StopWatches,
            Command::StartMoving => Mark::StartMoving,
            Command::AskFinal { windows, behind, .. } => {
                Mark::AskFinal { windows: count(windows.len()), behind: count(behind.len()) }
            }
            Command::Swap { generation, window } => Mark::Swap { of: *generation, window: *window },
            Command::Snap { window, frame } => Mark::Snap { window: *window, frame: *frame },
            Command::KeepFocusedLook { generation, window } => {
                Mark::KeepFocusedLook { of: *generation, window: *window }
            }
            Command::HandOverFocus { to, from } => Mark::HandOverFocus { to: *to, from: *from },
            Command::Lift { .. } => Mark::Lift { at_rest, landed },
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
        let delivery = Mark::Delivery { of, why, pictures: count(pictures.len()), sent_before_us: before(sent) };
        self.push(now, generation, delivery);
        for (window, frame, taken) in pictures {
            self.push(now, generation, Mark::Delivered { window, frame, taken_before_us: before(taken) });
        }
    }

    /// A request's press, stamped when the reactor took it and with the generation its first drive
    /// left.
    fn press(&mut self, cause: Cause, generation: Generation) {
        self.push(cause.at, generation, Mark::Press { what: cause.what });
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
    fn start_writing(&mut self) {
        self.written = self.written.max(self.first);
    }

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

/// The lifecycle with its journal, driven as one: the press each request answers, every event the
/// coordinator takes, every command it gives and every delivery of pictures the engine hears is marked,
/// and while the journal is written a burst is given once it settles.
#[derive(Default)]
pub struct Journalled {
    coordinator: Coordinator,
    journal: Journal,
    writing: bool,
    /// Between a `ReportUp` and its `ReportSettled`, when nothing is written.
    up: bool,
    /// The press of the request being handled, until its first drive marks it.
    pressed: Option<Cause>,
}

impl Journalled {
    pub fn coordinator(&self) -> &Coordinator {
        &self.coordinator
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
        let burst = if self.writing && settled { self.journal.unwritten() } else { None };
        (commands, burst)
    }

    /// The engine takes a request answering `cause`: its next drive marks the press, ahead of that
    /// event's own marks.
    pub fn pressed(&mut self, cause: Cause) {
        self.pressed = Some(cause);
    }

    /// The engine is done with the request: a press no drive marked, a request it declined, is
    /// dropped.
    pub fn answered(&mut self) {
        self.pressed = None;
    }

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
        self.journal.delivery(now, self.coordinator.generation(), of, why, sent, pictures);
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
        if self.up { None } else { self.journal.unwritten() }
    }
}

#[cfg(test)]
mod tests {
    use rini_geometry::space::{Point, Size};

    use super::*;
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

    fn extend(ms: u32) -> Mark {
        Mark::Extend { ms }
    }

    /// Every mark the journalled lifecycle holds.
    fn held(flights: &Journalled) -> Vec<Stamped> {
        flights.journal.ring.iter().copied().collect()
    }

    /// A press the reactor took `before` `at`.
    fn pressed(at: Instant, before: Duration) -> Cause {
        Cause { at: at - before, what: "move_focus" }
    }

    fn presses(marks: &[Stamped]) -> Vec<Stamped> {
        marks.iter().copied().filter(|stamped| matches!(stamped.mark, Mark::Press { .. })).collect()
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
            landing: frames.clone(),
            awaiting: Vec::new(),
            landing_until: Instant::now(),
            awaiting_until: Instant::now(),
            focus: Some((w(2), r(0.0, 0.0, 200.0, 200.0))),
            background: Vec::new(),
            background_initial: None,
            behind: vec![w(9)],
        };
        let snap = OverlayRect::new(Point::new(1.0, 2.0), Size::new(3.0, 4.0));
        let table = [
            (Command::Hold, "hold"),
            (Command::Settle, "settle"),
            (Command::Release, "release"),
            (Command::AskFrameZero { generation: g, strip: frames.clone(), background: Vec::new(), behind: Vec::new() }, "ask_frame_zero"),
            (Command::Show, "show"),
            (Command::Write(frames.clone()), "write"),
            (Command::Holes(vec![snap]), "holes"),
            (Command::Watch(watch.clone()), "watch"),
            (Command::StopWatches, "stop_watches"),
            (Command::StartMoving, "start_moving"),
            (Command::AskFinal { generation: g, windows: frames.clone(), behind: Vec::new() }, "ask_final"),
            (Command::Swap { generation: g, window: w(1) }, "swap"),
            (Command::Snap { window: w(1), frame: snap }, "snap"),
            (Command::KeepFocusedLook { generation: g, window: w(1) }, "keep_focused_look"),
            (Command::HandOverFocus { to: w(2), from: Some(w(1)) }, "hand_over_focus"),
            (Command::Lift { generation: g }, "lift"),
            (Command::Warm(frames.clone()), "warm"),
            (Command::Refresh(frames), "refresh"),
            (Command::ReportUp, "report_up"),
            (Command::ReportSettled, "report_settled"),
        ];
        for (command, expected) in table {
            assert_eq!(name(&Journal::mark(&command, (true, false))), expected, "{command:?}");
        }
        assert_eq!(
            Journal::mark(&Command::Watch(watch), (false, false)),
            Mark::Watch { landing: 1, awaiting: 0, background: 0, focus: Some(w(2)) }
        );
        assert_eq!(Journal::mark(&Command::Lift { generation: g }, (true, false)), Mark::Lift { at_rest: true, landed: false });
    }

    #[test]
    fn every_event_but_picture_and_frame_zero_has_its_mark() {
        let g = Generation(1);
        let frame = r(0.0, 0.0, 200.0, 200.0);
        assert_eq!(marks_of(Event::Begin(pan())), ["begin"]);
        assert_eq!(marks_of(Event::Merge { stage: pan(), moved: true }), ["merge"]);
        assert_eq!(marks_of(Event::Lost { generation: g, window: w(1) }), ["lost"]);
        assert_eq!(marks_of(Event::Extend(Duration::from_millis(5))), ["extend"]);
        assert_eq!(marks_of(Event::Tick { settled: true, landed: true }), ["at_rest", "landed"]);
        assert_eq!(marks_of(Event::Quiet(g)), ["quiet"]);
        assert_eq!(marks_of(Event::Abandon), ["abandon"]);
        assert_eq!(marks_of(Event::LooksTurned(vec![(w(1), frame), (w(2), frame)])), ["looks_turned", "looks_turned"]);
        assert!(marks_of(Event::FrameZero(g)).is_empty(), "the delivery is the record");
        assert!(marks_of(Event::Picture { generation: g, window: w(1), frame, why: Why::Landed }).is_empty());

        let Pending::One(begin) = Journal::before(&Event::Begin(pan()), false) else { panic!() };
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
        flights.drive(Event::Tick { settled: false, landed: false }, now + Duration::from_millis(240));
        let before = held(&flights).len();
        let end = now + PLAYS;
        flights.drive(Event::Tick { settled: true, landed: false }, end);
        flights.drive(Event::Tick { settled: true, landed: false }, end + Duration::from_millis(16));
        flights.drive(Event::Tick { settled: true, landed: true }, end + Duration::from_millis(32));
        let marks = held(&flights);
        assert_eq!(names(&marks[before..]), ["at_rest", "landed", "stop_watches", "lift", "settle"]);
        assert_eq!(marks[before].at, end, "stamped with the tick that first read it");
    }

    #[test]
    fn a_tick_before_the_clock_runs_out_marks_nothing() {
        let now = Instant::now();
        let mut flights = Journalled::default();
        flights.drive(Event::Begin(pan()), now);
        flights.drive(Event::FrameZero(flights.coordinator().generation()), now);
        let before = held(&flights).len();
        flights.drive(Event::Tick { settled: true, landed: true }, now + Duration::from_millis(10));
        assert_eq!(held(&flights).len(), before, "the engine read nothing, whatever the tick says");
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
        flights.drive(Event::Merge { stage: pan(), moved: false }, end + Duration::from_millis(5));
        let merged = flights.coordinator().generation();
        assert_ne!(first, merged);
        flights.drive(Event::Tick { settled: false, landed: true }, end + Duration::from_millis(16));
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
            held(&flights).into_iter().find(|stamped| matches!(stamped.mark, Mark::Lift { .. })).map(|stamped| stamped.mark)
        };
        assert_eq!(lift(true, true, now + PLAYS), Some(Mark::Lift { at_rest: true, landed: true }));
        assert_eq!(lift(true, false, now + PLAYS + LIFT_GRACE), Some(Mark::Lift { at_rest: true, landed: false }), "overdue");
        assert_eq!(lift(false, false, now + PLAYS + LIFT_GRACE), Some(Mark::Lift { at_rest: false, landed: false }));
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
        flights.delivery(heard, first, Why::Landed, heard - Duration::from_micros(4_100), pictures.into_iter());
        let marks = &held(&flights)[before..];
        assert_eq!(marks.iter().map(|stamped| stamped.mark).collect::<Vec<_>>(), [
            Mark::Delivery { of: first, why: Why::Landed, pictures: 2, sent_before_us: 4_100 },
            Mark::Delivered { window: w(1), frame: frame(0.0), taken_before_us: 37_500 },
            Mark::Delivered { window: w(2), frame: frame(864.0), taken_before_us: 37_000 },
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
        flights.delivery(now + Duration::from_millis(30), g, Why::FrameZero, now + Duration::from_millis(28), std::iter::empty());
        let marks = &held(&flights)[before..];
        assert_eq!(marks.iter().map(|stamped| stamped.mark).collect::<Vec<_>>(), [
            Mark::Delivery { of: g, why: Why::FrameZero, pictures: 0, sent_before_us: 2_000 },
        ]);
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
        assert_eq!(press, Stamped { at: cause.at, generation: Generation(2), mark });
        assert!(press.at < journal.ring[0].at, "pushed after a mark it came before: `t` is unsorted");
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
        assert_eq!(burst.marks.iter().map(|stamped| stamped.mark).collect::<Vec<_>>(), [extend(3), extend(4)]);
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
        assert_eq!(burst.lost, 0, "what it overwrote while off was never going to be written");
        assert_eq!(burst.marks[0].mark, extend(5));
    }

    #[test]
    fn the_anchor_converts_either_side_of_it() {
        let base = Instant::now();
        let anchor = Anchor { at: base + Duration::from_secs(1), uptime_ns: 5_000_000_000 };
        assert_eq!(anchor.ns(anchor.at), 5_000_000_000);
        assert_eq!(anchor.ns(anchor.at + Duration::from_millis(3)), 5_003_000_000);
        assert_eq!(anchor.ns(anchor.at - Duration::from_micros(2)), 4_999_998_000);
        assert_eq!(Anchor { at: anchor.at, uptime_ns: 5 }.ns(base), 0, "never before the clock's start");
    }

    #[test]
    fn a_line_is_one_object_with_t_g_and_mark() {
        let at = Instant::now();
        let anchor = Anchor { at, uptime_ns: 81_234_567_890_123 };
        let stamped = |mark| Stamped { at: at + Duration::from_micros(5), generation: Generation(42), mark };
        let frame = OverlayRect::new(Point::new(0.0, 32.0), Size::new(864.0, 1085.5));
        assert_eq!(
            line(&anchor, &stamped(Mark::Snap { window: w(3104), frame })),
            r#"{"t":81234567895123,"g":42,"mark":"snap","window":[1,3104],"frame":[0.0,32.0,864.0,1085.5]}"#
        );
        assert_eq!(line(&anchor, &stamped(Mark::Hold)), r#"{"t":81234567895123,"g":42,"mark":"hold"}"#);
        assert_eq!(
            line(&anchor, &stamped(Mark::Press { what: "focus_window" })),
            r#"{"t":81234567895123,"g":42,"mark":"press","what":"focus_window"}"#
        );
        assert_eq!(
            line(&anchor, &stamped(Mark::HandOverFocus { to: w(2), from: None })),
            r#"{"t":81234567895123,"g":42,"mark":"hand_over_focus","to":[1,2],"from":null}"#
        );
        assert_eq!(
            line(&anchor, &stamped(Mark::Merge {
                kind: Kind::LayoutPass,
                moved: false,
                tiles: 2,
                awaiting: 1,
                repainting: 0,
                focus: Some(w(7)),
                focused_look: None,
                duration_ms: 350,
            })),
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
            line(&anchor, &stamped(Mark::Delivery {
                of: Generation(42),
                why: Why::FrameZero,
                pictures: 1,
                sent_before_us: 4_100,
            })),
            r#"{"t":81234567895123,"g":42,"mark":"delivery","of":42,"why":"frame_zero","pictures":1,"sent_before_us":4100}"#
        );
        let taken_at = r(0.0, 32.0, 864.0, 1085.0);
        assert_eq!(
            line(&anchor, &stamped(Mark::Delivered { window: w(3104), frame: taken_at, taken_before_us: 31_200 })),
            r#"{"t":81234567895123,"g":42,"mark":"delivered","window":[1,3104],"frame":[0.0,32.0,864.0,1085.0],"taken_before_us":31200}"#
        );
        let whys = [Why::FrameZero, Why::Landed, Why::FocusChanged, Why::Background, Why::Final];
        assert_eq!(
            whys.map(|why| serde_json::to_value(why).unwrap()),
            ["frame_zero", "landed", "focus_changed", "background", "final"]
        );
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
        let found: Vec<String> = names(&marks).into_iter().filter(|name| order.contains(&name.as_str())).collect();
        assert_eq!(found, order);
        assert!(marks.iter().all(|stamped| stamped.generation == Generation(1)), "one generation: {marks:?}");
        assert_eq!(
            names(&marks[..5]),
            ["press", "begin", "hold", "ask_frame_zero", "report_up"],
            "the press, the event, then its commands"
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
        assert_eq!(presses(&marks), [Stamped { at: cause.at, generation: first, mark }]);

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
        assert_eq!(presses(marks).len(), 1, "once, by the first of the request's drives");
        assert_eq!(marks[0].generation, merged, "in the generation its drive left");
    }

    #[test]
    fn a_request_answered_with_no_drive_marks_no_press() {
        let now = Instant::now();
        let mut flights = Journalled::default();
        flights.pressed(pressed(now, Duration::from_millis(1)));
        flights.answered();
        flights.drive(Event::LooksTurned(vec![(w(1), r(0.0, 0.0, 200.0, 200.0))]), now);
        flights.drive(Event::Begin(pan()), now);
        let marks = held(&flights);
        assert!(presses(&marks).is_empty(), "{marks:?}");
        assert_eq!(names(&marks[..1]), ["looks_turned"]);
    }

    #[test]
    fn a_settled_burst_is_given_once_while_writing() {
        let mut flights = Journalled::default();
        assert_eq!(flights.set_writing(true), None, "an empty ring");
        let bursts = fly(&mut flights, Instant::now());
        assert!(bursts[..3].iter().all(Option::is_none), "nothing is written while a burst is up");
        let burst = bursts[3].as_ref().expect("given at its settle");
        assert_eq!(burst.marks, held(&flights), "every mark of it, its own settle included");
        assert_eq!(name(&burst.marks.last().unwrap().mark), "report_settled");
        assert_eq!(flights.drive(Event::Quiet(Generation(1)), Instant::now()).1, None, "once");
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
        assert_eq!(names(&burst.marks[..1]), ["begin"], "from before writing turned on");
        assert_eq!(burst.marks, held(&flights));
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
