//! Captures of windows on screen, through SkyLight: what a transition asks for at frame zero and just
//! before its lift, and the watches that capture a window as it lands where it ends, as its look
//! changes after a focus report, and as the windows behind the strip repaint. A window those watches
//! wait for that ends cut by the display's edge, which SkyLight cannot picture whole, is handed to
//! ScreenCaptureKit (`Outcome::clipped`, `Outcome::elsewhere`).
//!
//! Pure. Frames and prints come in with `now`; the windows to look at, and what to deliver to whom, go
//! out. `crate::pictures::domain::schedule` orders this work against ScreenCaptureKit's, and
//! `crate::pictures::platform::service` reads the frames, makes the calls and prints the pictures. The
//! rules and why each holds: "The watches" in `src/pictures/docs/README.md`.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use rini_core::ids::{WindowId, WindowServerId};
use rini_geometry::space::{ScreenPoint, ScreenRect};
use serde::Serialize;

use crate::pictures::domain::print::print_changed;
use crate::pictures::domain::settle::repaint_settled;
use crate::pictures::domain::target::PictureTarget;

/// A window whose server frame is this close to where it ends, in each of x, y, width and height,
/// has landed: the window server rounds each window on its own, and an app may settle a point or two
/// off what it was asked.
pub const END_SLACK: f64 = 20.0;
/// A window this close to where its tile starts is where frame zero expects it.
const FRAME_ZERO_SLACK: f64 = 2.0;
/// A frame this far past a display's edge still counts as on it.
const ON_DISPLAY_SLACK: f64 = 1.0;
/// How often a window not yet landed has its server frame read.
pub const LANDING_POLL: Duration = Duration::from_millis(1);
/// How often, and how many times, a window whose look turned with the key window is captured until
/// its look changes: by a `Change` watch in flight, and by a refresh at rest
/// (`crate::pictures::domain::refresh`).
pub const CHANGE_EVERY: Duration = Duration::from_millis(15);
pub const CHANGE_TRIES: u32 = 6;
/// How often, and how many times at most, a window that has to repaint is captured once it is where
/// it ends: about a second. See "A grow waits for its repaint" in
/// `src/transitions/docs/coordination.md`.
const REPAINT_EVERY: Duration = Duration::from_millis(8);
const REPAINT_TRIES: usize = 125;
/// How many times at most a window that has to repaint, where it ends but cut by the display's edge,
/// is captured through ScreenCaptureKit, one capture after another: about a second at 38-56ms a
/// capture, its listings besides.
const ELSEWHERE_TRIES: usize = 20;
/// The least time between two refreshes of the windows behind the strip.
pub const LIVE_EVERY: Duration = Duration::from_millis(16);
/// A refresh behind the strip waits for a change watch's target capture due this soon.
const LIVE_YIELDS_WITHIN: Duration = Duration::from_millis(8);
/// How long a focus report is remembered for a watch created after it.
const FOCUS_REMEMBERED: Duration = Duration::from_secs(1);

/// A client of the service, as the application numbers them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Client(pub u32);

/// Who asked, and for which of its generations: echoed on every delivery, so a client takes only its
/// own pictures, and a transition only those of its current end state.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Tag {
    pub client: Client,
    pub generation: u64,
}

/// Why a picture was delivered.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Why {
    /// Asked for at frame zero.
    FrameZero,
    /// The window landed where it ends.
    Landed,
    /// The window's look changed after it was reported focused.
    FocusChanged,
    /// A window behind the strip, refreshed.
    Background,
    /// Asked for just before the lift.
    Final,
}

/// When in a transition windows on screen are asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Moment {
    /// Before the overlay goes up, of the windows where their tiles start: every picture is delivered,
    /// in one delivery, even an empty one, so the overlay never waits for a window that is not there.
    FrameZero,
    /// Shortly before the lift, of the windows where they end: only pictures that show something new.
    Final,
}

/// Windows on screen now, captured at the frames the window server has them at and delivered
/// together under `tag`.
#[derive(Clone, Debug)]
pub struct OnScreen {
    pub tag: Tag,
    pub moment: Moment,
    /// The strip's windows, each with the frame it is expected at.
    pub strip: Vec<(PictureTarget, ScreenRect)>,
    /// Windows behind the strip that stay put. Captured in calls of their own: in one composite with
    /// the strip, a translucent window would bake what is behind it into its picture.
    pub background: Vec<(PictureTarget, ScreenRect)>,
    /// Windows below the strip, listed after it in the baked call so its materials blur them.
    pub behind: Vec<WindowServerId>,
}

/// A watch the service runs until it is stopped.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct WatchId(pub u64);

/// When a landed window's picture is delivered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Settle {
    /// The first capture once the window is there.
    AsSoonAsThere,
    /// The first capture whose rendering has settled, by `repaint_settled`: for a window a transition
    /// resizes or opens, or draws with no picture of its own, not painted at its new size yet. Captured
    /// every `REPAINT_EVERY`, `REPAINT_TRIES` times at most once there; one there but cut by the
    /// display's edge, which SkyLight cannot picture whole, through ScreenCaptureKit instead,
    /// `ELSEWHERE_TRIES` times at most (`Outcome::elsewhere`).
    Repainted,
}

#[derive(Clone, Debug)]
pub enum Watch {
    /// Each window captured once the window server has it within `slack` of where it ends and wholly on
    /// the display, or, repainting, cut by its edge; one not there by `until` is reported lost.
    Landing {
        tag: Tag,
        windows: Vec<(PictureTarget, ScreenRect)>,
        behind: Vec<WindowServerId>,
        slack: f64,
        settle: Settle,
        until: Instant,
    },
    /// Armed by the window's focus report: captured where it ends every `every`, `tries` times at most,
    /// until its band print changes from the last picture delivered of it as the watch starts, even
    /// when another watch delivered the changed look first, or until the cache refuses a capture; an
    /// end not there yet is looked for every `LANDING_POLL` between those rounds. `losing`, the window
    /// focus leaves and where it ends, is captured in the same rounds and done the same way on its
    /// own, but never handed to ScreenCaptureKit, and reported lost when it is done with no change;
    /// the watch is done when both are.
    Change {
        tag: Tag,
        window: PictureTarget,
        end: ScreenRect,
        losing: Option<(PictureTarget, ScreenRect)>,
        behind: Vec<WindowServerId>,
        every: Duration,
        tries: u32,
    },
    /// Windows behind the strip, refreshed every `every`, never ahead of a landing or a change watch's
    /// target capture, and only once the `after` watch's target is done, the window losing focus
    /// perhaps still watched; but once at `initial` whatever `after` says.
    Live {
        tag: Tag,
        windows: Vec<PictureTarget>,
        every: Duration,
        after: Option<WatchId>,
        initial: Option<Instant>,
    },
}

/// What a client tells the service of a window, as it learns it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hint {
    /// The window server reports the window focused.
    Focused(WindowId),
    /// A frame was written to the window: read where it is now, rather than at the next poll.
    Placed(WindowId),
}

/// What a window must be, where the window server has it, to be captured.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Accept {
    /// Within `slack` of `end`, and wholly on the display.
    Near { end: ScreenRect, slack: f64 },
    /// With its origin within `FRAME_ZERO_SLACK` of `at`, and wholly on the display.
    Origin { at: ScreenPoint },
    /// Wholly on the display, wherever.
    OnDisplay,
}

impl Accept {
    pub fn accepts(&self, frame: ScreenRect, display: ScreenRect) -> bool {
        wholly_on(frame, display)
            && match self {
                Self::Near { end, slack } => near(frame, *end, *slack),
                Self::Origin { at } => {
                    (frame.origin.x - at.x).abs() <= FRAME_ZERO_SLACK
                        && (frame.origin.y - at.y).abs() <= FRAME_ZERO_SLACK
                }
                Self::OnDisplay => true,
            }
    }
}

/// Whether `frame` lies wholly on `display`: SkyLight captures only what is on a display.
pub fn wholly_on(frame: ScreenRect, display: ScreenRect) -> bool {
    frame.size.width > 0.0
        && frame.size.height > 0.0
        && frame.origin.x >= display.origin.x - ON_DISPLAY_SLACK
        && frame.origin.y >= display.origin.y - ON_DISPLAY_SLACK
        && frame.origin.x + frame.size.width
            <= display.origin.x + display.size.width + ON_DISPLAY_SLACK
        && frame.origin.y + frame.size.height
            <= display.origin.y + display.size.height + ON_DISPLAY_SLACK
}

/// Whether `frame` is cut by `display`'s edge: partly on it, not wholly.
fn clipped(frame: ScreenRect, display: ScreenRect) -> bool {
    frame.intersection(&display).is_some() && !wholly_on(frame, display)
}

/// Whether `a` is within `slack` of `b` in each of x, y, width and height.
pub fn near(a: ScreenRect, b: ScreenRect, slack: f64) -> bool {
    (a.origin.x - b.origin.x).abs() <= slack
        && (a.origin.y - b.origin.y).abs() <= slack
        && (a.size.width - b.size.width).abs() <= slack
        && (a.size.height - b.size.height).abs() <= slack
}

/// One window to look at: captured when its server frame meets any of `accepts`.
#[derive(Clone, Debug, PartialEq)]
pub struct Look {
    pub target: PictureTarget,
    pub accepts: Vec<Accept>,
    /// Its thumbprint is wanted too, for a rendering that has to settle.
    pub thumbprint: bool,
}

/// Windows captured together, never in one composite with another set's.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Set {
    pub looks: Vec<Look>,
    pub behind: Vec<WindowServerId>,
}

/// What the window server and the capture said of one window looked at.
#[derive(Clone, Debug, PartialEq)]
pub struct Seen {
    pub window: WindowId,
    /// Its server frame, `None` when it has none.
    pub frame: Option<ScreenRect>,
    pub capture: Capture,
}

/// What came of capturing a window looked at. A watch decides only on pictures the cache took, so
/// what it delivers, and the prints it judges news by, are what its client was given.
#[derive(Clone, Debug, PartialEq)]
pub enum Capture {
    /// No picture: the window was not where it was wanted, or the call brought none back.
    Missed,
    /// A picture the cache refused for the one it holds, or in a refresh's round would refuse.
    Refused,
    /// A picture the cache took, or in a refresh's round would take, with its prints.
    Taken(Prints),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Prints {
    pub band: Vec<u8>,
    pub thumb: Option<Vec<u8>>,
}

/// Pictures to hand over under one tag, each with the server frame it was taken at.
#[derive(Clone, Debug, PartialEq)]
pub struct Delivery {
    pub tag: Tag,
    pub why: Why,
    pub windows: Vec<(WindowId, ScreenRect)>,
}

/// What a round of looking decided.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Outcome {
    pub deliveries: Vec<Delivery>,
    /// Windows a landing watch gave up on, and a window losing focus whose change a change watch
    /// will not see.
    pub lost: Vec<(Tag, WindowId)>,
    /// A changing window cut by the display's edge, which SkyLight cannot picture whole: one capture of
    /// it is owed through ScreenCaptureKit, delivered as its focus change.
    pub clipped: Vec<(Tag, PictureTarget, ScreenRect)>,
    /// A repainting window where it ends but cut by the display's edge: one capture of it is owed
    /// through ScreenCaptureKit, what came of it told back (`Watches::captured_elsewhere`).
    pub elsewhere: Vec<PictureTarget>,
}

/// What came of the ScreenCaptureKit capture a repainting window was owed (`Outcome::elsewhere`).
#[derive(Clone, Debug, PartialEq)]
pub enum CapturedElsewhere {
    /// No picture, or none with a thumbprint.
    Missed,
    /// A picture the cache refused for the one it holds.
    Refused,
    /// A picture the cache took, with its thumbprint.
    Taken(Vec<u8>),
}

impl Outcome {
    fn deliver(&mut self, tag: Tag, why: Why, window: WindowId, frame: ScreenRect) {
        match self.deliveries.iter_mut().find(|d| d.tag == tag && d.why == why) {
            Some(delivery) => delivery.windows.push((window, frame)),
            None => self.deliveries.push(Delivery {
                tag,
                why,
                windows: vec![(window, frame)],
            }),
        }
    }
}

/// Every ask and watch of windows on screen, and the prints of what each client was last delivered.
#[derive(Default)]
pub struct Watches {
    asks: Vec<OnScreen>,
    watches: Vec<(WatchId, Watching)>,
    focused: Vec<(WindowId, Instant)>,
    prints: HashMap<(Client, WindowId), Vec<u8>>,
    /// The tag whose frame zero each client's prints were last started afresh by.
    started: HashMap<Client, Tag>,
    plan: Plan,
    /// Windows losing focus a retarget carried on given up on, to report lost under its new tag.
    lost: Vec<(Tag, WindowId)>,
}

/// What the round handed out was for.
#[derive(Default)]
struct Plan {
    asks: Vec<OnScreen>,
    due: Vec<(WatchId, WindowId)>,
    /// The change watches looked at for their round, not only polled.
    rounds: Vec<WatchId>,
}

enum Watching {
    Landing(Landing),
    Change(Change),
    Live(Live),
}

struct Landing {
    tag: Tag,
    behind: Vec<WindowServerId>,
    slack: f64,
    settle: Settle,
    until: Instant,
    windows: Vec<Lands>,
    poll: Instant,
}

struct Lands {
    target: PictureTarget,
    end: ScreenRect,
    state: Arrival,
}

#[derive(PartialEq)]
enum Arrival {
    /// Not where it ends yet.
    Pending,
    /// There, and captured until its rendering settles.
    Settling {
        left: usize,
        last: Option<Vec<u8>>,
        next: Instant,
    },
    /// There at `frame` but cut by the display's edge, and captured through ScreenCaptureKit, one
    /// capture owed or out at a time, until its rendering settles.
    Elsewhere {
        left: usize,
        last: Option<Vec<u8>>,
        frame: ScreenRect,
    },
    Arrived,
    Lost,
}

struct Change {
    tag: Tag,
    target: Turning,
    losing: Option<Turning>,
    behind: Vec<WindowServerId>,
    every: Duration,
    armed: bool,
    /// When its ends not done are next looked at, together.
    next: Instant,
    /// When its ends the last look missed are looked for again, before `next`.
    poll: Option<Instant>,
}

/// One end of a `Change` watch.
struct Turning {
    target: PictureTarget,
    /// The band print a change is judged from: what the client was delivered of the window as the
    /// watch started; with none then, what it was delivered by the first capture, else that capture.
    baseline: Option<Vec<u8>>,
    end: ScreenRect,
    left: u32,
    /// Its look changed, its tries ran out, the cache refused its capture, it was handed to
    /// ScreenCaptureKit, or, the window losing focus, it was found clipped.
    done: bool,
    /// Its look changed.
    turned: bool,
    /// The last look found it not where it ends.
    missed: bool,
}

impl Turning {
    fn new(target: PictureTarget, end: ScreenRect, baseline: Option<Vec<u8>>, tries: u32) -> Self {
        Self { target, baseline, end, left: tries, done: tries == 0, turned: false, missed: false }
    }

    /// A try spent: with none left it is done.
    fn spend(&mut self) {
        self.left = self.left.saturating_sub(1);
        self.done = self.left == 0;
    }

    /// Carries on from `old`, the same window's, while its end keeps its size: a band print is drawn
    /// from the window's whole width, so one taken at another size never matches, and the cache
    /// refuses a capture only at the size of what it holds. Whether it did.
    fn carry_on(&mut self, old: Turning) -> bool {
        let kept = self.target.window == old.target.window && self.end.size == old.end.size;
        if kept {
            self.baseline = old.baseline;
            self.left = old.left.min(self.left);
            self.done = old.done;
            self.turned = old.turned;
            self.missed = old.missed;
        }
        kept
    }
}

impl Change {
    /// Its ends, the window losing focus first: its look turns first on screen.
    fn ends(&self) -> impl Iterator<Item = &Turning> {
        self.losing.iter().chain([&self.target])
    }

    fn end_of(&mut self, window: WindowId) -> Option<&mut Turning> {
        self.losing.iter_mut().chain([&mut self.target]).find(|end| end.target.window == window)
    }

    fn done(&self) -> bool {
        self.ends().all(|end| end.done)
    }

    /// When it is next looked at: its round, or a poll before it while an end the last look missed is
    /// still watched. A poll left by an end forgotten or retargeted since would hand out nothing.
    fn wake(&self) -> Instant {
        match self.poll {
            Some(poll) if self.ends().any(|end| !end.done && end.missed) => poll.min(self.next),
            _ => self.next,
        }
    }

    fn due(&self, now: Instant) -> bool {
        self.wake() <= now
    }

    /// When its target is next looked at, while it is not done.
    fn target_wake(&self) -> Option<Instant> {
        let target = &self.target;
        (!target.done).then(|| match self.poll {
            Some(poll) if target.missed => poll.min(self.next),
            _ => self.next,
        })
    }
}

struct Live {
    tag: Tag,
    targets: Vec<PictureTarget>,
    every: Duration,
    after: Option<WatchId>,
    initial: Option<Instant>,
    last: Option<Instant>,
}

impl Watches {
    pub fn ask(&mut self, ask: OnScreen) {
        self.asks.push(ask)
    }

    /// Starts `watch` as `id`.
    pub fn watch(&mut self, id: WatchId, watch: Watch, now: Instant) {
        self.watches.retain(|(held, _)| *held != id);
        let watching = self.start(watch, now);
        self.watches.push((id, watching));
    }

    /// Replaces what `id` watches, keeping what it already knows: a window that landed where it still
    /// ends stays landed, one settling there through ScreenCaptureKit goes on settling, a focus report
    /// already seen stays seen, a change watch at the same size keeps its baseline, its tries and
    /// whether it is done, the window losing focus given up on reported lost again under the new tag,
    /// and the background keeps its pace. A window watched to
    /// land somewhere new starts its print afresh: what the client was delivered of it was taken for
    /// the end before, which the client no longer draws.
    pub fn retarget(&mut self, id: WatchId, watch: Watch, now: Instant) {
        let Some(at) = self.watches.iter().position(|(held, _)| *held == id) else {
            return self.watch(id, watch, now);
        };
        let fresh = self.start(watch, now);
        let old = std::mem::replace(&mut self.watches[at].1, fresh);
        match (&mut self.watches[at].1, old) {
            (Watching::Landing(new), Watching::Landing(mut old)) => {
                for lands in &mut new.windows {
                    let Some(kept) =
                        old.windows.iter_mut().find(|o| o.target.window == lands.target.window)
                    else {
                        continue;
                    };
                    if kept.end != lands.end {
                        self.prints.remove(&(new.tag.client, lands.target.window));
                    } else if matches!(kept.state, Arrival::Arrived | Arrival::Elsewhere { .. }) {
                        lands.state = std::mem::replace(&mut kept.state, Arrival::Pending);
                    }
                }
            }
            (Watching::Change(new), Watching::Change(old))
                if new.target.target.window == old.target.target.window =>
            {
                new.armed = old.armed;
                if new.target.carry_on(old.target) {
                    new.next = old.next;
                    new.poll = old.poll;
                }
                let retagged = new.tag != old.tag;
                if let (Some(losing), Some(old)) = (&mut new.losing, old.losing)
                    && losing.carry_on(old)
                    && losing.done
                    && !losing.turned
                    && retagged
                {
                    self.lost.push((new.tag, losing.target.window));
                }
            }
            (Watching::Live(new), Watching::Live(old)) => new.last = old.last,
            _ => {}
        }
    }

    pub fn stop(&mut self, id: WatchId) {
        self.watches.retain(|(held, _)| *held != id)
    }

    pub fn hint(&mut self, hint: Hint, now: Instant) {
        match hint {
            Hint::Focused(window) => {
                self.focused.retain(|(held, at)| {
                    *held != window && now.saturating_duration_since(*at) <= FOCUS_REMEMBERED
                });
                self.focused.push((window, now));
                for (_, watching) in &mut self.watches {
                    if let Watching::Change(change) = watching
                        && change.target.target.window == window
                        && !change.armed
                    {
                        change.armed = true;
                        change.next = now;
                    }
                }
            }
            Hint::Placed(window) => {
                for (_, watching) in &mut self.watches {
                    match watching {
                        Watching::Landing(landing)
                            if landing.windows.iter().any(|l| {
                                l.target.window == window && l.state == Arrival::Pending
                            }) =>
                        {
                            landing.poll = now;
                        }
                        Watching::Change(change)
                            if change.armed
                                && change.ends().any(|end| end.target.window == window && !end.done) =>
                        {
                            change.next = change.next.min(now);
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    /// A window that is gone: nothing more is asked of it, and nothing held of it is compared again.
    pub fn forget(&mut self, window: WindowId) {
        for ask in &mut self.asks {
            ask.strip.retain(|(target, _)| target.window != window);
            ask.background.retain(|(target, _)| target.window != window);
        }
        self.watches.retain_mut(|(_, watching)| match watching {
            Watching::Landing(landing) => {
                landing.windows.retain(|l| l.target.window != window);
                true
            }
            Watching::Change(change) => {
                change.losing = change.losing.take().filter(|end| end.target.window != window);
                change.target.target.window != window
            }
            Watching::Live(live) => {
                live.targets.retain(|target| target.window != window);
                true
            }
        });
        self.focused.retain(|(held, _)| *held != window);
        self.prints.retain(|(_, held), _| *held != window);
    }

    /// Windows a landing watch waited for until its deadline, each reported once, and the windows
    /// losing focus a retarget carried on given up on.
    pub fn expire(&mut self, now: Instant) -> Vec<(Tag, WindowId)> {
        let mut lost = std::mem::take(&mut self.lost);
        for (_, watching) in &mut self.watches {
            if let Watching::Landing(landing) = watching
                && now >= landing.until
            {
                for lands in &mut landing.windows {
                    if lands.state == Arrival::Pending {
                        lands.state = Arrival::Lost;
                        lost.push((landing.tag, lands.target.window));
                    }
                }
            }
        }
        lost
    }

    /// The windows to look at now, if any: a frame-zero ask, one at a time, first; then every capture
    /// of the strip due, the final ask's among them; then the windows behind the strip.
    pub fn next(&mut self, now: Instant) -> Option<Vec<Set>> {
        if let Some(at) = self.asks.iter().position(|ask| ask.moment == Moment::FrameZero) {
            let ask = self.asks.remove(at);
            let sets = ask_sets(&ask);
            self.plan = Plan {
                asks: vec![ask],
                ..Plan::default()
            };
            return Some(sets);
        }

        let mut sets: Vec<Set> = Vec::new();
        let mut due = Vec::new();
        let mut rounds = Vec::new();
        for (id, watching) in &self.watches {
            match watching {
                Watching::Change(change) if change.armed && !change.done() && change.due(now) => {
                    let round = change.next <= now;
                    if round {
                        rounds.push(*id);
                    }
                    for end in change.ends().filter(|end| !end.done && (round || end.missed)) {
                        let accept = Accept::Near {
                            end: end.end,
                            slack: END_SLACK,
                        };
                        add(&mut sets, &change.behind, look(end.target, accept, false));
                        due.push((*id, end.target.window));
                    }
                }
                Watching::Landing(landing) => {
                    for lands in &landing.windows {
                        let wanted = match &lands.state {
                            Arrival::Pending => landing.poll <= now,
                            Arrival::Settling { next, .. } => *next <= now,
                            Arrival::Elsewhere { .. } | Arrival::Arrived | Arrival::Lost => false,
                        };
                        if wanted {
                            let accept = Accept::Near {
                                end: lands.end,
                                slack: landing.slack,
                            };
                            let thumbprint = landing.settle == Settle::Repainted;
                            add(
                                &mut sets,
                                &landing.behind,
                                look(lands.target, accept, thumbprint),
                            );
                            due.push((*id, lands.target.window));
                        }
                    }
                }
                _ => {}
            }
        }
        let finals: Vec<OnScreen> = {
            let (finals, rest) = std::mem::take(&mut self.asks)
                .into_iter()
                .partition(|ask| ask.moment == Moment::Final);
            self.asks = rest;
            finals
        };
        for ask in &finals {
            for set in ask_sets(ask) {
                for look in set.looks {
                    add(&mut sets, &set.behind, look);
                }
            }
        }
        if !sets.is_empty() {
            self.plan = Plan { asks: finals, due, rounds };
            return Some(sets);
        }

        let mut background = Set::default();
        for (id, watching) in &self.watches {
            if let Watching::Live(live) = watching
                && self.live_due(live, now).is_some_and(|at| at <= now)
            {
                for target in &live.targets {
                    add_to(&mut background, look(*target, Accept::OnDisplay, false));
                    due.push((*id, target.window));
                }
            }
        }
        if background.looks.is_empty() {
            return None;
        }
        self.plan = Plan { due, ..Plan::default() };
        Some(vec![background])
    }

    /// When something is next due, if anything is.
    pub fn wake(&self, now: Instant) -> Option<Instant> {
        let mut at: Option<Instant> = (!self.asks.is_empty() || !self.lost.is_empty()).then_some(now);
        let mut take = |t: Instant| at = Some(at.map_or(t, |at| at.min(t)));
        for (_, watching) in &self.watches {
            match watching {
                Watching::Change(change) if change.armed && !change.done() => take(change.wake()),
                Watching::Landing(landing) => {
                    for lands in &landing.windows {
                        match &lands.state {
                            Arrival::Pending => {
                                take(landing.poll);
                                take(landing.until);
                            }
                            Arrival::Settling { next, .. } => take(*next),
                            Arrival::Elsewhere { .. } | Arrival::Arrived | Arrival::Lost => {}
                        }
                    }
                }
                Watching::Live(live) => {
                    if let Some(due) = self.live_due(live, now) {
                        take(due);
                    }
                }
                Watching::Change(_) => {}
            }
        }
        at
    }

    /// What the window server and the captures said of the windows `next` handed out.
    pub fn seen(&mut self, now: Instant, seen: &[Seen], display: Option<ScreenRect>) -> Outcome {
        let plan = std::mem::take(&mut self.plan);
        let of = |window: WindowId| seen.iter().find(|s| s.window == window);
        // The window's server frame, where `accept` takes it, and what its capture came to.
        let looked_at = |window: WindowId, accept: Accept| {
            let seen = of(window)?;
            let frame = seen.frame?;
            display
                .is_some_and(|display| accept.accepts(frame, display))
                .then_some((frame, &seen.capture))
        };
        let taken = |window: WindowId, accept: Accept| match looked_at(window, accept)? {
            (frame, Capture::Taken(prints)) => Some((frame, prints)),
            (_, Capture::Missed | Capture::Refused) => None,
        };
        let mut out = Outcome::default();

        for ask in plan.asks {
            let client = ask.tag.client;
            // A transition's frame zero asked again, for what a merge brought on screen, adds to it.
            if ask.moment == Moment::FrameZero && self.started.insert(client, ask.tag) != Some(ask.tag) {
                self.prints.retain(|(held, _), _| *held != client);
            }
            let mut delivered = Vec::new();
            for (target, expected) in ask.strip.iter().chain(&ask.background) {
                let Some((frame, prints)) = taken(target.window, accept_for(ask.moment, *expected))
                else {
                    continue;
                };
                let new = news(&mut self.prints, client, target.window, &prints.band);
                if ask.moment == Moment::FrameZero || new {
                    delivered.push((target.window, frame));
                }
            }
            if ask.moment == Moment::FrameZero || !delivered.is_empty() {
                out.deliveries.push(Delivery {
                    tag: ask.tag,
                    why: ask.moment.why(),
                    windows: delivered,
                });
            }
        }

        // Each change watch looked at, and whether this look captured any of its ends.
        let mut changed: Vec<(WatchId, bool)> = Vec::new();
        for (id, window) in plan.due {
            let Some(at) = self.watches.iter().position(|(held, _)| *held == id) else {
                continue;
            };
            match &mut self.watches[at].1 {
                Watching::Landing(landing) => {
                    let (tag, settle, slack) = (landing.tag, landing.settle, landing.slack);
                    if let Some(lands) =
                        landing.windows.iter_mut().find(|l| l.target.window == window)
                    {
                        let cut = of(window).and_then(|seen| seen.frame).zip(display).filter(|(frame, display)| {
                            near(*frame, lands.end, slack) && clipped(*frame, *display)
                        });
                        match cut {
                            Some((frame, _)) if settle == Settle::Repainted => {
                                let left = ELSEWHERE_TRIES;
                                lands.state = Arrival::Elsewhere { left, last: None, frame };
                                out.elsewhere.push(lands.target);
                            }
                            _ => {
                                let there = looked_at(window, Accept::Near { end: lands.end, slack });
                                land(lands, settle, tag, there, now, &mut self.prints, &mut out);
                            }
                        }
                    }
                    if landing.windows.iter().any(|l| l.state == Arrival::Pending) {
                        landing.poll = now + LANDING_POLL;
                    }
                }
                Watching::Change(change) => {
                    let tag = change.tag;
                    let losing = change.losing.as_ref().is_some_and(|end| end.target.window == window);
                    let Some(end) = change.end_of(window) else { continue };
                    let accept = Accept::Near {
                        end: end.end,
                        slack: END_SLACK,
                    };
                    end.missed = false;
                    match looked_at(window, accept) {
                        Some((frame, Capture::Taken(prints))) => {
                            let client = tag.client;
                            // A landing may have delivered the changed look first: the change is
                            // judged from what was delivered before it, not from the latest delivery.
                            let judged = end
                                .baseline
                                .take()
                                .or_else(|| self.prints.get(&(client, window)).cloned());
                            news(&mut self.prints, client, window, &prints.band);
                            if let Some(baseline) = judged.as_deref()
                                && print_changed(Some(baseline), &prints.band)
                            {
                                end.done = true;
                                end.turned = true;
                                out.deliver(tag, Why::FocusChanged, window, frame);
                            } else {
                                // With nothing delivered to judge a change by, the first capture is
                                // delivered as the window where it ends, and the change judged from it.
                                if judged.is_none() {
                                    out.deliver(tag, Why::Landed, window, frame);
                                }
                                end.baseline = Some(judged.unwrap_or_else(|| prints.band.clone()));
                                end.spend();
                            }
                        }
                        // Every later capture at this size is refused too.
                        Some((_, Capture::Refused)) => end.done = true,
                        Some((_, Capture::Missed)) | None => {
                            let frame = of(window).and_then(|seen| seen.frame);
                            if let (Some(frame), Some(display)) = (frame, display)
                                && near(frame, end.end, END_SLACK)
                                && clipped(frame, display)
                            {
                                end.done = true;
                                if !losing {
                                    out.clipped.push((tag, end.target, frame));
                                }
                            } else {
                                end.missed = true;
                            }
                        }
                    }
                    if losing && end.done && !end.turned {
                        out.lost.push((tag, window));
                    }
                    let captured = !end.missed;
                    match changed.iter_mut().find(|(held, _)| *held == id) {
                        Some((_, any)) => *any |= captured,
                        None => changed.push((id, captured)),
                    }
                }
                Watching::Live(live) => {
                    live.last = Some(now);
                    live.initial = None;
                    if let Some((frame, prints)) = taken(window, Accept::OnDisplay) {
                        news(&mut self.prints, live.tag.client, window, &prints.band);
                        out.deliver(live.tag, Why::Background, window, frame);
                    }
                }
            }
        }
        // Looked at again together `every` after a round that captured either end, and every
        // `LANDING_POLL` for an end not there yet: a poll leaves the round where it was, however long
        // its look took.
        for (id, captured) in changed {
            if let Some((_, Watching::Change(change))) = self.watches.iter_mut().find(|(held, _)| *held == id) {
                if plan.rounds.contains(&id) {
                    change.next = now + if captured { change.every } else { LANDING_POLL };
                }
                let missed = change.ends().any(|end| !end.done && end.missed);
                change.poll = missed.then_some(now + LANDING_POLL);
            }
        }
        out
    }

    /// What came of the ScreenCaptureKit capture `window` was owed, settling where it ends cut by the
    /// display's edge: delivered as landed, at the frame it was found there, once its thumbprint
    /// matches the capture's before it; lost when the cache refuses it or its tries run out; else
    /// owed another. Its band print is not taken from a surface, so the client's print of it is
    /// dropped as it is delivered.
    pub fn captured_elsewhere(&mut self, window: WindowId, capture: CapturedElsewhere) -> Outcome {
        let mut out = Outcome::default();
        for (_, watching) in &mut self.watches {
            let Watching::Landing(landing) = watching else { continue };
            let tag = landing.tag;
            let Some(lands) = landing.windows.iter_mut().find(|l| l.target.window == window) else {
                continue;
            };
            let Arrival::Elsewhere { left, last, frame } = &mut lands.state else { continue };
            let frame = *frame;
            match &capture {
                CapturedElsewhere::Taken(thumb) if repaint_settled(last.as_deref(), thumb) => {
                    lands.state = Arrival::Arrived;
                    self.prints.remove(&(tag.client, window));
                    out.deliver(tag, Why::Landed, window, frame);
                }
                CapturedElsewhere::Refused => {
                    lands.state = Arrival::Lost;
                    out.lost.push((tag, window));
                }
                _ if *left <= 1 => {
                    lands.state = Arrival::Lost;
                    out.lost.push((tag, window));
                }
                capture => {
                    *left -= 1;
                    *last = match capture {
                        CapturedElsewhere::Taken(thumb) => Some(thumb.clone()),
                        _ => None,
                    };
                    out.elsewhere.push(lands.target);
                }
            }
        }
        out
    }

    fn start(&self, watch: Watch, now: Instant) -> Watching {
        match watch {
            Watch::Landing {
                tag,
                windows,
                behind,
                slack,
                settle,
                until,
            } => Watching::Landing(Landing {
                tag,
                behind,
                slack,
                settle,
                until,
                windows: windows
                    .into_iter()
                    .map(|(target, end)| Lands {
                        target,
                        end,
                        state: Arrival::Pending,
                    })
                    .collect(),
                poll: now,
            }),
            Watch::Change {
                tag,
                window,
                end,
                losing,
                behind,
                every,
                tries,
            } => {
                let armed = self.focused.iter().any(|(held, at)| {
                    *held == window.window && now.saturating_duration_since(*at) <= FOCUS_REMEMBERED
                });
                let turning = |(target, end): (PictureTarget, ScreenRect)| {
                    let baseline = self.prints.get(&(tag.client, target.window)).cloned();
                    Turning::new(target, end, baseline, tries)
                };
                Watching::Change(Change {
                    tag,
                    target: turning((window, end)),
                    losing: losing.map(turning),
                    behind,
                    every,
                    armed,
                    next: now,
                    poll: None,
                })
            }
            Watch::Live {
                tag,
                windows,
                every,
                after,
                initial,
            } => Watching::Live(Live {
                tag,
                targets: windows,
                every,
                after,
                initial,
                last: None,
            }),
        }
    }

    /// When `live` is next due, unless something it never runs ahead of is pending: a window still
    /// landing, or a change watch's target capture due within `LIVE_YIELDS_WITHIN`. The window losing
    /// focus does not: one never where it ends is polled for every `LANDING_POLL` to the flight's end.
    fn live_due(&self, live: &Live, now: Instant) -> Option<Instant> {
        let landing = self.watches.iter().any(|(_, watching)| {
            matches!(watching, Watching::Landing(landing)
                if landing.windows.iter().any(|l| matches!(l.state, Arrival::Pending | Arrival::Settling { .. })))
        });
        let change_soon = self.watches.iter().any(|(_, watching)| {
            matches!(watching, Watching::Change(change)
                if change.armed && change.target_wake().is_some_and(|at| at <= now + LIVE_YIELDS_WITHIN))
        });
        if landing || change_soon || live.targets.is_empty() {
            return None;
        }
        let regular = self
            .after_done(live.after)
            .then(|| live.last.map_or(now, |last| last + live.every));
        match (regular, live.initial) {
            (Some(regular), Some(initial)) => Some(regular.min(initial)),
            (regular, initial) => regular.or(initial),
        }
    }

    /// Whether the watch `after` names has delivered its target's change, run out of it, or gone:
    /// the window losing focus may go on alone.
    fn after_done(&self, after: Option<WatchId>) -> bool {
        let Some(after) = after else { return true };
        self.watches.iter().all(|(id, watching)| {
            *id != after || !matches!(watching, Watching::Change(change) if !change.target.done)
        })
    }
}

/// Whether `band` is news to `client` of `window`, which it is then told of.
fn news(
    prints: &mut HashMap<(Client, WindowId), Vec<u8>>,
    client: Client,
    window: WindowId,
    band: &[u8],
) -> bool {
    let new = print_changed(prints.get(&(client, window)).map(Vec::as_slice), band);
    prints.insert((client, window), band.to_vec());
    new
}

/// What one look at a landing window decides: delivered once there (or once settled there, for a
/// repainting one); lost when a repainting one runs out of tries, or when the cache refuses its
/// capture.
fn land(
    lands: &mut Lands,
    settle: Settle,
    tag: Tag,
    there: Option<(ScreenRect, &Capture)>,
    now: Instant,
    prints: &mut HashMap<(Client, WindowId), Vec<u8>>,
    out: &mut Outcome,
) {
    let window = lands.target.window;
    let refused = matches!(there, Some((_, Capture::Refused)));
    let there = there.and_then(|(frame, capture)| match capture {
        Capture::Taken(seen) => Some((frame, seen)),
        Capture::Missed | Capture::Refused => None,
    });
    let (left, last) = match std::mem::replace(&mut lands.state, Arrival::Arrived) {
        done @ (Arrival::Elsewhere { .. } | Arrival::Arrived | Arrival::Lost) => {
            lands.state = done;
            return;
        }
        // Every later capture at this size is refused too: what the cache holds is the picture.
        _ if refused => {
            lands.state = Arrival::Lost;
            out.lost.push((tag, window));
            return;
        }
        Arrival::Pending if there.is_none() => {
            lands.state = Arrival::Pending;
            return;
        }
        Arrival::Pending => (REPAINT_TRIES, None),
        Arrival::Settling { left, last, .. } => (left, last),
    };
    if settle == Settle::AsSoonAsThere {
        if let Some((frame, seen)) = there
            && news(prints, tag.client, window, &seen.band)
        {
            out.deliver(tag, Why::Landed, window, frame);
        }
        return;
    }
    let thumb = there.and_then(|(_, seen)| seen.thumb.clone());
    let settled = thumb
        .as_deref()
        .is_some_and(|thumb| repaint_settled(last.as_deref(), thumb));
    match there {
        Some((frame, seen)) if settled => {
            news(prints, tag.client, window, &seen.band);
            out.deliver(tag, Why::Landed, window, frame);
        }
        _ if left <= 1 => {
            lands.state = Arrival::Lost;
            out.lost.push((tag, window));
        }
        _ => {
            lands.state = Arrival::Settling {
                left: left - 1,
                last: thumb,
                next: now + REPAINT_EVERY,
            }
        }
    }
}

impl Moment {
    pub fn why(self) -> Why {
        match self {
            Self::FrameZero => Why::FrameZero,
            Self::Final => Why::Final,
        }
    }
}

fn accept_for(moment: Moment, expected: ScreenRect) -> Accept {
    match moment {
        Moment::FrameZero => Accept::Origin { at: expected.origin },
        Moment::Final => Accept::Near {
            end: expected,
            slack: END_SLACK,
        },
    }
}

/// An ask's windows as the sets it is captured in: the strip with what is behind it, the background
/// on its own.
fn ask_sets(ask: &OnScreen) -> Vec<Set> {
    let looks = |windows: &[(PictureTarget, ScreenRect)]| {
        windows
            .iter()
            .map(|(target, expected)| look(*target, accept_for(ask.moment, *expected), false))
            .collect()
    };
    let mut sets = vec![Set {
        looks: looks(&ask.strip),
        behind: ask.behind.clone(),
    }];
    if !ask.background.is_empty() {
        sets.push(Set {
            looks: looks(&ask.background),
            behind: Vec::new(),
        });
    }
    sets
}

fn look(target: PictureTarget, accept: Accept, thumbprint: bool) -> Look {
    Look {
        target,
        accepts: vec![accept],
        thumbprint,
    }
}

/// Adds `look` to the set listing exactly `behind`, or a new one: a window's blur is of what is
/// composited after it, so windows with different `behind` are never in one call.
fn add(sets: &mut Vec<Set>, behind: &[WindowServerId], look: Look) {
    match sets.iter_mut().find(|set| set.behind == behind) {
        Some(set) => add_to(set, look),
        None => sets.push(Set {
            looks: vec![look],
            behind: behind.to_vec(),
        }),
    }
}

/// Adds `look` to `set`, folding it into the window's look when it is there already.
fn add_to(set: &mut Set, look: Look) {
    match set.looks.iter_mut().find(|held| held.target.window == look.target.window) {
        Some(held) => {
            for accept in look.accepts {
                if !held.accepts.contains(&accept) {
                    held.accepts.push(accept);
                }
            }
            held.thumbprint |= look.thumbprint;
        }
        None => set.looks.push(look),
    }
}

#[cfg(test)]
mod tests {
    use rini_geometry::space::ScreenSize;

    use super::*;

    const LAPTOP: ScreenRect = rect(0.0, 0.0, 1728.0, 1117.0);

    const fn rect(x: f64, y: f64, w: f64, h: f64) -> ScreenRect {
        ScreenRect::new(ScreenPoint::new(x, y), ScreenSize::new(w, h))
    }

    fn wid(idx: u32) -> WindowId {
        WindowId::new(1, idx)
    }

    fn target(idx: u32) -> PictureTarget {
        PictureTarget {
            window: wid(idx),
            server_id: WindowServerId::new(idx),
            size: ScreenSize::new(859.0, 1081.0),
        }
    }

    fn tag(generation: u64) -> Tag {
        Tag { client: Client(1), generation }
    }

    const LEFT: ScreenRect = rect(4.0, 32.0, 859.0, 1081.0);
    const RIGHT: ScreenRect = rect(865.0, 32.0, 859.0, 1081.0);
    /// Parked past the right edge.
    const PARKED: ScreenRect = rect(1727.0, 32.0, 859.0, 1081.0);

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    /// The windows a round looks at, by set.
    fn looked(sets: &[Set]) -> Vec<Vec<WindowId>> {
        sets.iter()
            .map(|set| set.looks.iter().map(|look| look.target.window).collect())
            .collect()
    }

    /// One round: what `next` hands out, and what `seen` makes of each window being where `at` says,
    /// captured and taken by the cache with the band print `band` says (`None`: not captured), on the
    /// laptop.
    fn round(
        watches: &mut Watches,
        now: Instant,
        at: impl Fn(WindowId) -> Option<ScreenRect>,
        band: impl Fn(WindowId) -> Option<u8>,
    ) -> Option<(Vec<Set>, Outcome)> {
        round_of(watches, now, at, |window| band(window).map_or(Capture::Missed, taken))
    }

    /// One round in which what came of each capture is what `capture` says.
    fn round_of(
        watches: &mut Watches,
        now: Instant,
        at: impl Fn(WindowId) -> Option<ScreenRect>,
        capture: impl Fn(WindowId) -> Capture,
    ) -> Option<(Vec<Set>, Outcome)> {
        let sets = watches.next(now)?;
        let seen: Vec<Seen> = sets
            .iter()
            .flat_map(|set| &set.looks)
            .map(|look| {
                let window = look.target.window;
                let frame = at(window);
                let captured =
                    frame.is_some_and(|f| look.accepts.iter().any(|a| a.accepts(f, LAPTOP)));
                Seen {
                    window,
                    frame,
                    capture: if captured { capture(window) } else { Capture::Missed },
                }
            })
            .collect();
        let outcome = watches.seen(now, &seen, Some(LAPTOP));
        Some((sets, outcome))
    }

    /// A capture the cache took with band print `band`; thumbprints 50 levels apart per step, past the
    /// settle tolerance.
    fn taken(band: u8) -> Capture {
        Capture::Taken(Prints {
            band: vec![band],
            thumb: Some(vec![band.wrapping_mul(50)]),
        })
    }

    fn delivered(outcome: &Outcome) -> Vec<(Why, Vec<WindowId>)> {
        outcome
            .deliveries
            .iter()
            .map(|d| (d.why, d.windows.iter().map(|(w, _)| *w).collect()))
            .collect()
    }

    fn frame_zero(strip: &[(u32, ScreenRect)], background: &[(u32, ScreenRect)]) -> OnScreen {
        OnScreen {
            tag: tag(1),
            moment: Moment::FrameZero,
            strip: strip.iter().map(|(idx, at)| (target(*idx), *at)).collect(),
            background: background.iter().map(|(idx, at)| (target(*idx), *at)).collect(),
            behind: vec![WindowServerId::new(9)],
        }
    }

    fn landing(windows: &[(u32, ScreenRect)], settle: Settle, until: Instant) -> Watch {
        landing_of(tag(1), vec![WindowServerId::new(9)], windows, settle, until)
    }

    fn landing_of(
        tag: Tag,
        behind: Vec<WindowServerId>,
        windows: &[(u32, ScreenRect)],
        settle: Settle,
        until: Instant,
    ) -> Watch {
        Watch::Landing {
            tag,
            windows: windows.iter().map(|(idx, end)| (target(*idx), *end)).collect(),
            behind,
            slack: END_SLACK,
            settle,
            until,
        }
    }

    fn change(idx: u32, end: ScreenRect) -> Watch {
        Watch::Change {
            tag: tag(1),
            window: target(idx),
            end,
            losing: None,
            behind: vec![WindowServerId::new(9)],
            every: CHANGE_EVERY,
            tries: CHANGE_TRIES,
        }
    }

    /// Window 2 gaining focus where it ends at `RIGHT`, window 1 losing it where it ends at `losing`.
    fn changing(losing: ScreenRect) -> Watch {
        match change(2, RIGHT) {
            Watch::Change { tag, window, end, behind, every, tries, .. } => Watch::Change {
                tag,
                window,
                end,
                losing: Some((target(1), losing)),
                behind,
                every,
                tries,
            },
            _ => unreachable!(),
        }
    }

    fn live(windows: &[u32], after: Option<WatchId>, initial: Option<Instant>) -> Watch {
        Watch::Live {
            tag: tag(1),
            windows: windows.iter().map(|idx| target(*idx)).collect(),
            every: LIVE_EVERY,
            after,
            initial,
        }
    }

    /// Frame zero names windows where their tiles start: the strip in one set with what is behind it,
    /// the background in its own, never one composite. Whatever is there within 2pt and wholly on the
    /// display is delivered, all in one delivery.
    #[test]
    fn frame_zero_delivers_every_window_where_it_starts_in_one_delivery() {
        let mut watches = Watches::default();
        watches.ask(frame_zero(
            &[(1, LEFT), (2, RIGHT), (3, PARKED)],
            &[(4, rect(100.0, 100.0, 400.0, 300.0))],
        ));
        let now = Instant::now();
        let moved = rect(
            LEFT.origin.x + 3.0,
            LEFT.origin.y,
            LEFT.size.width,
            LEFT.size.height,
        );
        let (sets, outcome) = round(
            &mut watches,
            now,
            |w| {
                Some(match w.idx.get() {
                    1 => rect(5.5, 33.0, 859.0, 1081.0),
                    2 => moved,
                    3 => PARKED,
                    _ => rect(100.0, 100.0, 400.0, 300.0),
                })
            },
            |_| Some(7),
        )
        .unwrap();
        assert_eq!(looked(&sets), vec![vec![wid(1), wid(2), wid(3)], vec![wid(4)]]);
        assert_eq!(sets[0].behind, vec![WindowServerId::new(9)]);
        assert!(
            sets[1].behind.is_empty(),
            "the background composites over nothing of the strip's"
        );
        assert_eq!(
            delivered(&outcome),
            vec![(Why::FrameZero, vec![wid(1), wid(4)])],
            "1.5pt off is there; 3pt off is not; parked past the edge is not on the display"
        );
    }

    /// The overlay waits for frame zero's delivery, so one comes even with nothing in it.
    #[test]
    fn frame_zero_delivers_even_when_nothing_is_there() {
        let mut watches = Watches::default();
        watches.ask(frame_zero(&[(1, LEFT)], &[]));
        let (_, outcome) = round(&mut watches, Instant::now(), |_| None, |_| None).unwrap();
        assert_eq!(delivered(&outcome), vec![(Why::FrameZero, vec![])]);

        let mut watches = Watches::default();
        watches.ask(frame_zero(&[(1, LEFT)], &[]));
        let _ = watches.next(Instant::now());
        let outcome = watches.seen(Instant::now(), &[], None);
        assert_eq!(
            delivered(&outcome),
            vec![(Why::FrameZero, vec![])],
            "no display known"
        );
    }

    /// Frame zero goes first, whatever else is due.
    #[test]
    fn frame_zero_is_looked_at_before_anything_else() {
        let now = Instant::now();
        let mut watches = Watches::default();
        watches.watch(
            WatchId(1),
            landing(&[(1, LEFT)], Settle::AsSoonAsThere, now + ms(150)),
            now,
        );
        watches.ask(OnScreen {
            moment: Moment::Final,
            ..frame_zero(&[(2, RIGHT)], &[])
        });
        watches.ask(frame_zero(&[(3, LEFT)], &[]));
        let sets = watches.next(now).unwrap();
        assert_eq!(looked(&sets), vec![vec![wid(3)]]);
    }

    /// A landing window is looked at every millisecond until it is within 20pt of its end and wholly on
    /// the display, captured once there, and delivered when its band print is news.
    #[test]
    fn a_landing_window_is_captured_once_it_is_where_it_ends() {
        let start = Instant::now();
        let mut watches = Watches::default();
        watches.watch(
            WatchId(1),
            landing(&[(1, LEFT)], Settle::AsSoonAsThere, start + ms(150)),
            start,
        );

        let (_, outcome) = round(&mut watches, start, |_| Some(PARKED), |_| Some(7)).unwrap();
        assert!(outcome.deliveries.is_empty(), "still parked");
        assert!(watches.next(start).is_none(), "not again before the next poll");
        assert_eq!(watches.wake(start), Some(start + LANDING_POLL));

        let near = rect(
            LEFT.origin.x + 19.0,
            LEFT.origin.y,
            LEFT.size.width + 1.0,
            LEFT.size.height,
        );
        let (_, outcome) = round(&mut watches, start + ms(1), |_| Some(near), |_| Some(7)).unwrap();
        assert_eq!(delivered(&outcome), vec![(Why::Landed, vec![wid(1)])]);
        assert_eq!(
            outcome.deliveries[0].windows[0].1, near,
            "with the frame it was taken at"
        );
        assert!(watches.next(start + ms(50)).is_none(), "landed: no more looks");
        assert!(watches.expire(start + ms(200)).is_empty(), "and never lost");
    }

    /// A landing that shows nothing the client was not delivered already is not delivered again.
    #[test]
    fn a_landing_with_an_unchanged_band_print_is_not_delivered() {
        let start = Instant::now();
        let mut watches = Watches::default();
        watches.ask(frame_zero(&[(1, LEFT)], &[]));
        round(&mut watches, start, |_| Some(LEFT), |_| Some(7)).unwrap();
        watches.watch(
            WatchId(1),
            landing(&[(1, LEFT)], Settle::AsSoonAsThere, start + ms(150)),
            start,
        );
        let (_, outcome) = round(&mut watches, start, |_| Some(LEFT), |_| Some(7)).unwrap();
        assert!(outcome.deliveries.is_empty());
        assert!(watches.next(start + ms(5)).is_none(), "landed all the same");
    }

    /// Each transition's frame zero starts the client's prints afresh: a window flying the cache's
    /// picture this time is delivered when it lands even if it looks as it did last time.
    #[test]
    fn frame_zero_starts_the_clients_prints_afresh() {
        let start = Instant::now();
        let mut watches = Watches::default();
        watches.watch(
            WatchId(1),
            landing(&[(1, LEFT)], Settle::AsSoonAsThere, start + ms(150)),
            start,
        );
        round(&mut watches, start, |_| Some(LEFT), |_| Some(7)).unwrap();
        watches.stop(WatchId(1));

        watches.ask(frame_zero(&[(1, PARKED)], &[]));
        round(&mut watches, start, |_| Some(PARKED), |_| Some(7)).unwrap();
        watches.watch(
            WatchId(2),
            landing(&[(1, LEFT)], Settle::AsSoonAsThere, start + ms(150)),
            start,
        );
        let (_, outcome) = round(&mut watches, start, |_| Some(LEFT), |_| Some(7)).unwrap();
        assert_eq!(delivered(&outcome), vec![(Why::Landed, vec![wid(1)])]);
    }

    /// Frame zero asked again under the same tag, for windows a merged pass brought on screen, adds to
    /// the prints the first started rather than starting them afresh again.
    #[test]
    fn frame_zero_asked_again_under_its_tag_keeps_the_prints_the_first_started() {
        let start = Instant::now();
        let mut watches = Watches::default();
        watches.ask(frame_zero(&[(1, LEFT)], &[]));
        round(&mut watches, start, |_| Some(LEFT), |_| Some(7)).unwrap();
        watches.ask(frame_zero(&[(2, RIGHT)], &[]));
        round(&mut watches, start, |_| Some(RIGHT), |_| Some(7)).unwrap();
        watches.watch(
            WatchId(1),
            landing(&[(1, LEFT), (2, RIGHT)], Settle::AsSoonAsThere, start + ms(150)),
            start,
        );
        let at = |w: WindowId| Some(if w == wid(1) { LEFT } else { RIGHT });
        let (_, outcome) = round(&mut watches, start, at, |_| Some(7)).unwrap();
        assert!(outcome.deliveries.is_empty(), "neither is news: {outcome:?}");
    }

    /// A window not where it ends by the deadline is reported lost, once.
    #[test]
    fn a_window_not_landed_by_the_deadline_is_lost() {
        let start = Instant::now();
        let mut watches = Watches::default();
        watches.watch(
            WatchId(1),
            landing(&[(1, LEFT), (2, RIGHT)], Settle::AsSoonAsThere, start + ms(150)),
            start,
        );
        round(
            &mut watches,
            start,
            |w| (w == wid(2)).then_some(RIGHT),
            |_| Some(7),
        )
        .unwrap();
        assert!(watches.expire(start + ms(149)).is_empty());
        assert_eq!(watches.expire(start + ms(150)), vec![(tag(1), wid(1))]);
        assert!(watches.expire(start + ms(151)).is_empty(), "once");
        assert!(watches.next(start + ms(151)).is_none(), "and looked at no more");
    }

    /// A frame written to a window sends the next look at once instead of at the next poll.
    #[test]
    fn a_placed_window_is_looked_at_at_once() {
        let start = Instant::now();
        let mut watches = Watches::default();
        watches.watch(
            WatchId(1),
            landing(&[(1, LEFT)], Settle::AsSoonAsThere, start + ms(150)),
            start,
        );
        round(&mut watches, start + ms(10), |_| Some(PARKED), |_| Some(7)).unwrap();
        assert!(watches.next(start + ms(10)).is_none());
        watches.hint(Hint::Placed(wid(1)), start + ms(10));
        assert_eq!(
            looked(&watches.next(start + ms(10)).unwrap()),
            vec![vec![wid(1)]]
        );
    }

    /// A window the flight holds for is captured every 8ms once there, until two prints in a row match,
    /// and that one is delivered whatever its band print.
    #[test]
    fn a_repainting_landing_is_delivered_once_two_prints_in_a_row_match() {
        let start = Instant::now();
        let mut watches = Watches::default();
        watches.ask(frame_zero(&[(1, LEFT)], &[]));
        round(&mut watches, start, |_| Some(LEFT), |_| Some(3)).unwrap();
        watches.watch(
            WatchId(1),
            landing(&[(1, LEFT)], Settle::Repainted, start + ms(150)),
            start,
        );

        let painting = std::cell::Cell::new(1u8);
        let mut at = start;
        let mut rounds = 0;
        let outcome = loop {
            let (sets, outcome) =
                round(&mut watches, at, |_| Some(LEFT), |_| Some(painting.get())).unwrap();
            assert!(sets[0].looks[0].thumbprint, "its thumbprint is wanted");
            rounds += 1;
            if !outcome.deliveries.is_empty() {
                break outcome;
            }
            // Repaints for two captures, then holds still.
            if rounds < 3 {
                painting.set(painting.get() + 1);
            }
            assert!(watches.next(at + ms(7)).is_none(), "every 8ms");
            at += REPAINT_EVERY;
        };
        assert_eq!(rounds, 4, "1, 2, 3, then 3 again");
        assert_eq!(delivered(&outcome), vec![(Why::Landed, vec![wid(1)])]);
    }

    /// One that never settles is given up on after `REPAINT_TRIES` captures.
    #[test]
    fn a_repainting_landing_that_never_settles_is_lost() {
        let start = Instant::now();
        let mut watches = Watches::default();
        watches.watch(
            WatchId(1),
            landing(&[(1, LEFT)], Settle::Repainted, start + ms(150)),
            start,
        );
        let counter = std::cell::Cell::new(0u8);
        let mut at = start;
        let mut lost = Vec::new();
        for _ in 0..REPAINT_TRIES {
            let (_, outcome) = round(
                &mut watches,
                at,
                |_| Some(LEFT),
                |_| {
                    counter.set(counter.get().wrapping_add(1));
                    Some(counter.get())
                },
            )
            .unwrap();
            assert!(outcome.deliveries.is_empty());
            lost.extend(outcome.lost);
            at += REPAINT_EVERY;
        }
        assert_eq!(lost, vec![(tag(1), wid(1))]);
        assert!(watches.next(at).is_none());
        assert!(watches.expire(at + ms(500)).is_empty(), "lost once");
    }

    /// A landing whose capture the cache refuses for the picture it holds is lost at once, waiting or
    /// settling, and never delivered: every later capture at that size would be refused too.
    #[test]
    fn a_landing_the_cache_refuses_is_lost_at_once() {
        let start = Instant::now();
        let mut watches = Watches::default();
        watches.watch(
            WatchId(1),
            landing(&[(1, LEFT)], Settle::AsSoonAsThere, start + ms(150)),
            start,
        );
        watches.watch(
            WatchId(2),
            landing(&[(2, RIGHT)], Settle::Repainted, start + ms(150)),
            start,
        );
        let at = |w: WindowId| Some(if w == wid(1) { LEFT } else { RIGHT });
        let (_, outcome) = round_of(&mut watches, start, at, |w| {
            if w == wid(1) { Capture::Refused } else { taken(1) }
        })
        .unwrap();
        assert!(outcome.deliveries.is_empty(), "refused, and not settled yet");
        assert_eq!(outcome.lost, vec![(tag(1), wid(1))], "at once, not at the deadline");

        let next = start + REPAINT_EVERY;
        let (_, outcome) = round_of(&mut watches, next, at, |_| Capture::Refused).unwrap();
        assert!(outcome.deliveries.is_empty());
        assert_eq!(outcome.lost, vec![(tag(1), wid(2))], "settling, refused");
        assert!(watches.next(start + ms(50)).is_none(), "looked at no more");
        assert!(watches.expire(start + ms(150)).is_empty(), "each lost once");
    }

    /// Where a window ends cut by the left edge, as fold1's g18 unfold ends it.
    const CUT: ScreenRect = rect(-285.0, 32.0, 1148.0, 1081.0);

    /// A repainting window there, watched from `start`, handed to ScreenCaptureKit by the round that
    /// finds it there.
    fn handed_elsewhere(start: Instant) -> Watches {
        let mut watches = Watches::default();
        watches.watch(WatchId(1), landing(&[(1, CUT)], Settle::Repainted, start + ms(300)), start);
        let (sets, outcome) = round(&mut watches, start, |_| Some(CUT), |_| Some(1)).unwrap();
        assert!(sets[0].looks[0].thumbprint);
        assert!(outcome.deliveries.is_empty(), "SkyLight cannot picture it whole");
        assert_eq!(outcome.elsewhere, vec![target(1)]);
        watches
    }

    /// A repainting window where it ends but cut by the display's edge is captured through
    /// ScreenCaptureKit, one capture after another, no longer looked at through SkyLight, and
    /// delivered as landed, at the frame it was found at, once two captures in a row match. A window
    /// only landing there is never handed over: it is lost at its deadline.
    #[test]
    fn a_repainting_landing_cut_by_the_displays_edge_settles_on_captures_elsewhere() {
        let start = Instant::now();
        let mut watches = handed_elsewhere(start);
        assert!(watches.next(start + ms(50)).is_none() && watches.wake(start).is_none(), "nothing to poll");
        assert!(watches.expire(start + ms(300)).is_empty(), "there, so past its deadline");
        let outcome = watches.captured_elsewhere(wid(1), CapturedElsewhere::Taken(vec![1]));
        assert!(outcome.deliveries.is_empty());
        assert_eq!(outcome.elsewhere, vec![target(1)], "another, to judge it by");
        let outcome = watches.captured_elsewhere(wid(1), CapturedElsewhere::Taken(vec![1]));
        assert_eq!(outcome.deliveries, vec![Delivery { tag: tag(1), why: Why::Landed, windows: vec![(wid(1), CUT)] }]);
        assert!(outcome.elsewhere.is_empty());
        assert_eq!(watches.captured_elsewhere(wid(1), CapturedElsewhere::Taken(vec![1])), Outcome::default(), "done");

        let mut watches = Watches::default();
        watches.watch(WatchId(1), landing(&[(1, CUT)], Settle::AsSoonAsThere, start + ms(150)), start);
        let (_, outcome) = round(&mut watches, start, |_| Some(CUT), |_| Some(1)).unwrap();
        assert!(outcome.elsewhere.is_empty());
        assert_eq!(watches.expire(start + ms(150)), vec![(tag(1), wid(1))]);
    }

    /// One captured elsewhere is lost at once when the cache refuses a capture, and once
    /// `ELSEWHERE_TRIES` captures never settle, an empty capture spending a try too.
    #[test]
    fn a_repainting_landing_captured_elsewhere_is_lost_when_refused_or_out_of_tries() {
        let start = Instant::now();
        let mut watches = handed_elsewhere(start);
        let outcome = watches.captured_elsewhere(wid(1), CapturedElsewhere::Refused);
        assert_eq!((outcome.lost, outcome.elsewhere), (vec![(tag(1), wid(1))], Vec::new()));

        let mut watches = handed_elsewhere(start);
        let mut lost = Vec::new();
        for try_ in 0..ELSEWHERE_TRIES {
            let capture = if try_ == 0 { CapturedElsewhere::Missed } else { CapturedElsewhere::Taken(vec![(try_ as u8).wrapping_mul(50)]) };
            let outcome = watches.captured_elsewhere(wid(1), capture);
            assert!(outcome.deliveries.is_empty());
            assert_eq!(outcome.elsewhere.is_empty(), try_ == ELSEWHERE_TRIES - 1, "try {try_}");
            lost.extend(outcome.lost);
        }
        assert_eq!(lost, vec![(tag(1), wid(1))]);
    }

    /// A retarget to the same end carries a window settling elsewhere on, its capture out delivered
    /// under the new tag; one to a new end starts it afresh, and what comes back meanwhile is
    /// nobody's.
    #[test]
    fn a_retarget_carries_a_window_settling_elsewhere_on_while_it_ends_there() {
        let start = Instant::now();
        let mut watches = handed_elsewhere(start);
        watches.captured_elsewhere(wid(1), CapturedElsewhere::Taken(vec![1]));
        let retagged = |end| landing_of(tag(2), vec![WindowServerId::new(9)], &[(1, end)], Settle::Repainted, start + ms(300));
        watches.retarget(WatchId(1), retagged(CUT), start + ms(40));
        assert!(watches.next(start + ms(40)).is_none(), "not polled again");
        let outcome = watches.captured_elsewhere(wid(1), CapturedElsewhere::Taken(vec![1]));
        assert_eq!(outcome.deliveries, vec![Delivery { tag: tag(2), why: Why::Landed, windows: vec![(wid(1), CUT)] }]);

        let mut watches = handed_elsewhere(start);
        let wider = rect(-285.0, 32.0, 1400.0, 1081.0);
        watches.retarget(WatchId(1), retagged(wider), start + ms(40));
        assert_eq!(watches.captured_elsewhere(wid(1), CapturedElsewhere::Taken(vec![1])), Outcome::default());
        assert!(watches.next(start + ms(40)).is_some(), "looked for where it ends now");
    }

    /// The focus target is captured only once its focus is reported; from then every 15ms, six times
    /// at most, stopping at the first change of its band print.
    #[test]
    fn a_change_watch_captures_after_the_focus_report_until_the_look_changes() {
        let start = Instant::now();
        let mut watches = Watches::default();
        watches.ask(frame_zero(&[(1, LEFT)], &[]));
        round(&mut watches, start, |_| Some(LEFT), |_| Some(1)).unwrap();
        watches.watch(WatchId(1), change(1, LEFT), start);
        assert!(watches.next(start + ms(100)).is_none(), "no focus report");

        watches.hint(Hint::Focused(wid(1)), start + ms(100));
        let mut at = start + ms(100);
        for _ in 0..2 {
            let (_, outcome) = round(&mut watches, at, |_| Some(LEFT), |_| Some(1)).unwrap();
            assert!(outcome.deliveries.is_empty(), "unchanged");
            assert!(watches.next(at + ms(14)).is_none(), "every 15ms");
            at += CHANGE_EVERY;
        }
        let (_, outcome) = round(&mut watches, at, |_| Some(LEFT), |_| Some(2)).unwrap();
        assert_eq!(delivered(&outcome), vec![(Why::FocusChanged, vec![wid(1)])]);
        assert!(
            watches.next(at + ms(100)).is_none(),
            "stopped at the first change"
        );
    }

    /// With nothing delivered of the window to judge a change by, its first capture is delivered as
    /// the window where it ends, and the change judged from it; six tries in all.
    #[test]
    fn a_change_watch_with_nothing_to_judge_by_starts_from_its_first_capture_and_gives_up_after_six()
     {
        let start = Instant::now();
        let mut watches = Watches::default();
        watches.hint(Hint::Focused(wid(1)), start);
        watches.watch(WatchId(1), change(1, LEFT), start);
        let mut at = start;
        let mut outcomes = Vec::new();
        while let Some((_, outcome)) = round(&mut watches, at, |_| Some(LEFT), |_| Some(1)) {
            outcomes.push(outcome);
            at += CHANGE_EVERY;
        }
        assert_eq!(outcomes.len(), CHANGE_TRIES as usize);
        assert_eq!(
            delivered(&outcomes[0]),
            vec![(Why::Landed, vec![wid(1)])],
            "never a focus change"
        );
        assert!(outcomes[1..].iter().all(|outcome| outcome.deliveries.is_empty()));
    }

    /// A window that was focused before it landed: its landing, looked at first in the same round,
    /// delivers the focused look, and its change watch still sees it changed from frame zero's.
    #[test]
    fn a_change_watch_sees_the_look_a_landing_delivered_first() {
        let start = Instant::now();
        let mut watches = Watches::default();
        watches.ask(frame_zero(&[(1, RIGHT)], &[]));
        round(&mut watches, start, |_| Some(RIGHT), |_| Some(1)).unwrap();
        watches.hint(Hint::Focused(wid(1)), start);
        watches.watch(WatchId(1), landing(&[(1, LEFT)], Settle::AsSoonAsThere, start + ms(150)), start);
        watches.watch(WatchId(2), change(1, LEFT), start);
        let (_, outcome) = round(&mut watches, start, |_| Some(LEFT), |_| Some(2)).unwrap();
        assert_eq!(
            delivered(&outcome),
            vec![(Why::Landed, vec![wid(1)]), (Why::FocusChanged, vec![wid(1)])]
        );

        // A parked window, nothing delivered as the watch started: judged from what its landing
        // delivered by the watch's first capture.
        let mut watches = Watches::default();
        watches.hint(Hint::Focused(wid(1)), start);
        watches.watch(WatchId(1), landing(&[(1, LEFT)], Settle::AsSoonAsThere, start + ms(150)), start);
        watches.watch(WatchId(2), change(1, LEFT), start);
        let (_, outcome) = round(&mut watches, start, |_| Some(LEFT), |_| Some(1)).unwrap();
        assert_eq!(delivered(&outcome), vec![(Why::Landed, vec![wid(1)])], "one landing, delivered once");
        let (_, outcome) = round(&mut watches, start + CHANGE_EVERY, |_| Some(LEFT), |_| Some(2)).unwrap();
        assert_eq!(delivered(&outcome), vec![(Why::FocusChanged, vec![wid(1)])]);
    }

    /// The focus report often comes before the watch. Remembered for a second, it arms a watch
    /// created after it; older, it does not.
    #[test]
    fn a_focus_reported_just_before_the_watch_arms_it() {
        let start = Instant::now();
        let mut watches = Watches::default();
        watches.hint(Hint::Focused(wid(1)), start);
        watches.watch(WatchId(1), change(1, LEFT), start + ms(900));
        assert!(watches.next(start + ms(900)).is_some(), "900ms after the report");

        let mut watches = Watches::default();
        watches.hint(Hint::Focused(wid(1)), start);
        watches.watch(WatchId(1), change(1, LEFT), start + ms(1100));
        assert!(
            watches.next(start + ms(1100)).is_none(),
            "a report over a second old"
        );

        let mut watches = Watches::default();
        watches.hint(Hint::Focused(wid(2)), start);
        watches.watch(WatchId(1), change(1, LEFT), start);
        assert!(watches.next(start).is_none(), "another window's");
    }

    /// A change capture the cache refuses finishes the watch, delivering nothing: every later capture
    /// of the window at that size is refused too. The windows behind the strip that wait on it refresh
    /// at once, and nothing judges the window's look again, a retarget of it included.
    #[test]
    fn a_change_capture_the_cache_refuses_finishes_the_watch() {
        let start = Instant::now();
        let mut watches = Watches::default();
        watches.ask(frame_zero(&[(1, LEFT)], &[]));
        round(&mut watches, start, |_| Some(LEFT), |_| Some(1)).unwrap();
        watches.hint(Hint::Focused(wid(1)), start);
        watches.watch(WatchId(1), change(1, LEFT), start);
        watches.watch(WatchId(2), live(&[4], Some(WatchId(1)), None), start);
        let at = |w: WindowId| Some(if w == wid(1) { LEFT } else { RIGHT });
        let (_, outcome) = round(&mut watches, start, at, |_| Some(1)).unwrap();
        assert!(outcome.deliveries.is_empty(), "unchanged: a try spent");

        let next = start + CHANGE_EVERY;
        let (sets, outcome) = round_of(&mut watches, next, at, |_| Capture::Refused).unwrap();
        assert_eq!(looked(&sets), vec![vec![wid(1)]], "the background waits for it");
        assert!(outcome.deliveries.is_empty());
        assert_eq!(looked(&watches.next(next).unwrap()), vec![vec![wid(4)]], "done: the background now");
        watches.retarget(WatchId(1), change(1, LEFT), next);
        let later = next + CHANGE_EVERY;
        let (sets, _) = round(&mut watches, later, at, |_| Some(2)).unwrap();
        assert_eq!(looked(&sets), vec![vec![wid(4)]], "its look is never captured again");

        let mut watches = Watches::default();
        watches.hint(Hint::Focused(wid(1)), start);
        watches.watch(WatchId(1), change(1, LEFT), start);
        let (_, outcome) = round_of(&mut watches, start, |_| Some(LEFT), |_| Capture::Refused).unwrap();
        assert!(outcome.deliveries.is_empty(), "not delivered as landed either");
        assert!(watches.next(start + CHANGE_EVERY).is_none());
    }

    /// A change watch retargeted to an end of another size starts afresh but for its focus report: a
    /// refusal or the tries spent at one size say nothing of another. At the same size it carries on.
    #[test]
    fn a_change_watch_retargeted_to_another_size_starts_afresh_but_for_its_focus_report() {
        let start = Instant::now();
        let wider = rect(4.0, 32.0, 1200.0, 1081.0);
        let mut watches = Watches::default();
        watches.hint(Hint::Focused(wid(1)), start);
        watches.watch(WatchId(1), change(1, LEFT), start);
        round_of(&mut watches, start, |_| Some(LEFT), |_| Capture::Refused).unwrap();
        let later = start + ms(2000);
        watches.retarget(WatchId(1), change(1, rect(14.0, 32.0, 859.0, 1081.0)), later);
        assert!(watches.next(later).is_none(), "the same size: still done");
        watches.retarget(WatchId(1), change(1, wider), later);
        assert_eq!(
            looked(&watches.next(later).unwrap()),
            vec![vec![wid(1)]],
            "another size: captured again, armed long after the report"
        );

        let tries = |watches: &mut Watches, mut at: Instant, end: ScreenRect| {
            let mut tries = 0;
            while round(watches, at, |_| Some(end), |_| Some(1)).is_some() {
                tries += 1;
                at += CHANGE_EVERY;
            }
            tries
        };
        let mut watches = Watches::default();
        watches.hint(Hint::Focused(wid(1)), start);
        watches.watch(WatchId(1), change(1, LEFT), start);
        let at = start + CHANGE_EVERY * (CHANGE_TRIES - 1);
        let mut now = start;
        while now < at {
            round(&mut watches, now, |_| Some(LEFT), |_| Some(1)).unwrap();
            now += CHANGE_EVERY;
        }
        watches.retarget(WatchId(1), change(1, LEFT), at);
        assert_eq!(tries(&mut watches, at, LEFT), 1, "the same size: the one try left");
        watches.retarget(WatchId(1), change(1, wider), at);
        assert_eq!(tries(&mut watches, at, wider), CHANGE_TRIES, "another size: every try again");
    }

    /// Until the window is where it ends, a change watch only watches it land: no try is spent.
    #[test]
    fn a_change_watch_spends_no_try_before_its_window_is_there() {
        let start = Instant::now();
        let mut watches = Watches::default();
        watches.hint(Hint::Focused(wid(1)), start);
        watches.watch(WatchId(1), change(1, LEFT), start);
        let mut at = start;
        for _ in 0..20 {
            round(
                &mut watches,
                at,
                |_| PARKED.intersection(&LAPTOP).map(|on| on.union(&rect(900.0, 32.0, 1.0, 1.0))),
                |_| Some(1),
            );
            at += LANDING_POLL;
        }
        let mut tries = 0;
        while let Some(_) = round(&mut watches, at, |_| Some(LEFT), |_| Some(1)) {
            tries += 1;
            at += CHANGE_EVERY;
        }
        assert_eq!(tries, CHANGE_TRIES);
    }

    /// A focus target that ends cut by the display's edge cannot be pictured whole by SkyLight: it is
    /// owed one ScreenCaptureKit capture instead, and the watch is done.
    #[test]
    fn a_change_watch_on_a_clipped_end_asks_for_one_capture_elsewhere() {
        let start = Instant::now();
        let end = rect(1300.0, 32.0, 859.0, 1081.0);
        let mut watches = Watches::default();
        watches.hint(Hint::Focused(wid(1)), start);
        watches.watch(WatchId(1), change(1, end), start);
        let (_, outcome) = round(&mut watches, start, |_| Some(end), |_| Some(1)).unwrap();
        assert!(outcome.deliveries.is_empty());
        assert_eq!(outcome.clipped, vec![(tag(1), target(1), end)]);
        assert!(watches.next(start + ms(100)).is_none(), "once");
    }

    /// The window losing focus is captured in the same rounds as the target, in one set, listed first,
    /// each judged from its own baseline and done at its own change; a report of its own focus arms
    /// nothing.
    #[test]
    fn a_change_watch_captures_the_window_losing_focus_beside_the_target_each_done_at_its_own_change() {
        let start = Instant::now();
        let at = |w: WindowId| Some(if w == wid(1) { LEFT } else { RIGHT });
        let started = |watches: &mut Watches| {
            watches.ask(frame_zero(&[(1, LEFT), (2, RIGHT)], &[]));
            round(watches, start, at, |w| Some(if w == wid(1) { 1 } else { 5 })).unwrap();
            watches.watch(WatchId(1), changing(LEFT), start);
        };
        let mut watches = Watches::default();
        started(&mut watches);
        watches.hint(Hint::Focused(wid(1)), start);
        assert!(watches.next(start).is_none(), "armed by the target's report only");
        watches.hint(Hint::Focused(wid(2)), start);
        let (sets, outcome) = round(&mut watches, start, at, |w| Some(if w == wid(1) { 1 } else { 5 })).unwrap();
        assert_eq!(looked(&sets), vec![vec![wid(1), wid(2)]], "one set, the old window first");
        assert!(outcome.deliveries.is_empty());
        let next = start + CHANGE_EVERY;
        let (_, outcome) = round(&mut watches, next, at, |w| Some(if w == wid(1) { 2 } else { 5 })).unwrap();
        assert_eq!(delivered(&outcome), vec![(Why::FocusChanged, vec![wid(1)])], "from its own frame zero's");
        let next = next + CHANGE_EVERY;
        let (sets, outcome) = round(&mut watches, next, at, |w| Some(if w == wid(1) { 2 } else { 6 })).unwrap();
        assert_eq!(looked(&sets), vec![vec![wid(2)]], "done, it drops out");
        assert_eq!(delivered(&outcome), vec![(Why::FocusChanged, vec![wid(2)])]);
        assert!(watches.next(next + ms(100)).is_none(), "done when both are");

        let mut watches = Watches::default();
        started(&mut watches);
        watches.hint(Hint::Focused(wid(2)), start);
        let (_, outcome) = round(&mut watches, start, at, |w| Some(if w == wid(1) { 2 } else { 6 })).unwrap();
        assert_eq!(
            delivered(&outcome),
            vec![(Why::FocusChanged, vec![wid(1), wid(2)])],
            "in one round, the old window first"
        );
    }

    /// Each end spends its own tries, only on its own captures. While one is not there yet the other is
    /// looked at every 15ms, not polled with it every 1ms. The old window, out of tries with no change,
    /// is reported lost; the target never is.
    #[test]
    fn a_change_watch_spends_each_ends_tries_on_its_own_captures() {
        let start = Instant::now();
        let mut watches = Watches::default();
        watches.hint(Hint::Focused(wid(2)), start);
        watches.watch(WatchId(1), changing(LEFT), start);
        let landed_at = start + CHANGE_EVERY * 3;
        let mut at = start;
        let mut rounds = Vec::new();
        let mut lost = Vec::new();
        while let Some((sets, outcome)) = round(
            &mut watches,
            at,
            |w| Some(if w == wid(2) { RIGHT } else if at < landed_at { PARKED } else { LEFT }),
            |_| Some(1),
        ) {
            rounds.push(looked(&sets).concat());
            lost.extend(outcome.lost);
            at += CHANGE_EVERY;
        }
        let both = vec![wid(1), wid(2)];
        assert_eq!(rounds.len(), 3 + CHANGE_TRIES as usize, "{rounds:?}");
        assert!(rounds[..CHANGE_TRIES as usize].iter().all(|looked| *looked == both), "together, 15ms apart");
        assert!(
            rounds[CHANGE_TRIES as usize..].iter().all(|looked| *looked == vec![wid(1)]),
            "the old window's own six"
        );
        assert_eq!(lost, vec![(tag(1), wid(1))], "once, as its last try is spent");
    }

    /// An end not there yet is looked for every 1ms alone, while the end captured waits for the
    /// round 15ms after its capture, spending one try a round; once there, it joins that round.
    #[test]
    fn a_change_watch_polls_an_end_not_there_yet_alone_between_its_rounds() {
        let start = Instant::now();
        let mut watches = Watches::default();
        watches.hint(Hint::Focused(wid(2)), start);
        watches.watch(WatchId(1), changing(LEFT), start);
        let arrives = start + ms(5);
        let short = rect(700.0, 32.0, 859.0, 1081.0);
        let mut at = start;
        let mut rounds = Vec::new();
        while let Some(wake) = watches.wake(at) {
            at = wake;
            let (sets, _) = round(
                &mut watches,
                at,
                |w| Some(if w == wid(1) { LEFT } else if at < arrives { short } else { RIGHT }),
                |_| Some(1),
            )
            .unwrap();
            rounds.push((at - start, looked(&sets).concat()));
        }
        let (both, target) = (vec![wid(1), wid(2)], vec![wid(2)]);
        assert_eq!(rounds[0], (ms(0), both.clone()));
        assert_eq!(rounds[1..=5].iter().map(|(_, w)| w.clone()).collect::<Vec<_>>(), vec![target; 5], "{rounds:?}");
        assert_eq!(rounds[1].0, LANDING_POLL);
        assert_eq!(rounds[6], (CHANGE_EVERY, both.clone()), "there, it joins the round");
        let old = rounds.iter().filter(|(_, looked)| looked.contains(&wid(1))).count();
        assert_eq!(old, CHANGE_TRIES as usize, "the old window's tries, one a round");
        assert!(rounds[6..].iter().all(|(_, looked)| *looked == both));
    }

    /// A window losing focus that ends cut by the display's edge is done with no capture: the one
    /// ScreenCaptureKit capture a focus change lets through is the target's. Forgotten, it is
    /// watched no more, and the target still is.
    #[test]
    fn a_window_losing_focus_ending_clipped_is_done_with_no_capture_elsewhere() {
        let start = Instant::now();
        let cut = rect(1300.0, 32.0, 859.0, 1081.0);
        let mut watches = Watches::default();
        watches.hint(Hint::Focused(wid(2)), start);
        watches.watch(WatchId(1), changing(cut), start);
        let at = |w: WindowId| Some(if w == wid(1) { cut } else { RIGHT });
        let (_, outcome) = round(&mut watches, start, at, |_| Some(1)).unwrap();
        assert!(outcome.clipped.is_empty());
        assert_eq!(outcome.lost, vec![(tag(1), wid(1))], "its turn will not be seen");
        let (sets, _) = round(&mut watches, start + CHANGE_EVERY, at, |_| Some(1)).unwrap();
        assert_eq!(looked(&sets), vec![vec![wid(2)]]);

        let mut watches = Watches::default();
        watches.hint(Hint::Focused(wid(2)), start);
        watches.watch(WatchId(1), changing(LEFT), start);
        let at = |w: WindowId| Some(if w == wid(1) { LEFT } else { RIGHT });
        let (_, outcome) = round_of(&mut watches, start, at, |w| if w == wid(1) { Capture::Refused } else { taken(1) }).unwrap();
        assert_eq!(outcome.lost, vec![(tag(1), wid(1))], "refused, nor will it");

        let mut watches = Watches::default();
        watches.hint(Hint::Focused(wid(2)), start);
        watches.watch(WatchId(1), changing(LEFT), start);
        watches.forget(wid(1));
        assert_eq!(looked(&watches.next(start).unwrap()), vec![vec![wid(2)]]);
    }

    /// A poll lasts only while an end it is for is still missed: with that end forgotten, or
    /// retargeted to another size, the watch wakes at its round and not at a poll gone by.
    #[test]
    fn a_change_watch_with_no_end_missed_left_wakes_at_its_round() {
        let start = Instant::now();
        let short = rect(14.0, 32.0, 700.0, 1081.0);
        let missed = || {
            let mut watches = Watches::default();
            watches.hint(Hint::Focused(wid(2)), start);
            watches.watch(WatchId(1), changing(LEFT), start);
            round(&mut watches, start, |w| Some(if w == wid(1) { short } else { RIGHT }), |_| Some(1)).unwrap();
            assert_eq!(watches.wake(start), Some(start + LANDING_POLL), "the old window polled for");
            watches
        };
        let later = start + LANDING_POLL * 3;

        let mut watches = missed();
        watches.forget(wid(1));
        assert!(watches.next(later).is_none());
        assert_eq!(watches.wake(later), Some(start + CHANGE_EVERY), "forgotten");

        let mut watches = missed();
        watches.retarget(WatchId(1), changing(rect(4.0, 32.0, 860.0, 1081.0)), later);
        assert!(watches.next(later).is_none());
        assert_eq!(watches.wake(later), Some(start + CHANGE_EVERY), "at another size");
    }

    /// A window losing focus given up on is reported lost again when a retarget carries it on under a
    /// new tag: the client may have dropped the report under the tag before. Under the same tag, or
    /// once its look changed, it is not.
    #[test]
    fn a_window_losing_focus_given_up_on_is_reported_lost_again_under_a_retargets_new_tag() {
        let start = Instant::now();
        let cut = rect(1300.0, 32.0, 859.0, 1081.0);
        let under = |generation: u64, losing: ScreenRect| match changing(losing) {
            Watch::Change { window, end, losing, behind, every, tries, .. } => {
                Watch::Change { tag: tag(generation), window, end, losing, behind, every, tries }
            }
            _ => unreachable!(),
        };
        let mut watches = Watches::default();
        watches.hint(Hint::Focused(wid(2)), start);
        watches.watch(WatchId(1), under(1, cut), start);
        let at = |w: WindowId| Some(if w == wid(1) { cut } else { RIGHT });
        let (_, outcome) = round(&mut watches, start, at, |_| Some(1)).unwrap();
        assert_eq!(outcome.lost, vec![(tag(1), wid(1))]);
        let later = start + ms(2);
        watches.retarget(WatchId(1), under(1, cut), later);
        assert!(watches.expire(later).is_empty(), "the same tag");
        watches.retarget(WatchId(1), under(2, cut), later);
        assert_eq!(watches.wake(later), Some(later));
        assert_eq!(watches.expire(later), vec![(tag(2), wid(1))]);
        assert!(watches.expire(later).is_empty(), "once");

        let mut watches = Watches::default();
        watches.ask(frame_zero(&[(1, LEFT), (2, RIGHT)], &[]));
        round(&mut watches, start, |w| Some(if w == wid(1) { LEFT } else { RIGHT }), |_| Some(1)).unwrap();
        watches.hint(Hint::Focused(wid(2)), start);
        watches.watch(WatchId(1), under(1, LEFT), start);
        round(&mut watches, start, |w| Some(if w == wid(1) { LEFT } else { RIGHT }), |w| Some(if w == wid(1) { 2 } else { 1 }))
            .unwrap();
        watches.retarget(WatchId(1), under(2, LEFT), later);
        assert!(watches.expire(later).is_empty(), "its look changed");
    }

    /// A retarget keeps each end's baseline, tries and finish while that end keeps its size.
    #[test]
    fn a_change_watch_retargeted_carries_each_end_on_while_it_keeps_its_size() {
        let start = Instant::now();
        let at = |w: WindowId| Some(if w == wid(1) { LEFT } else { RIGHT });
        let mut watches = Watches::default();
        watches.ask(frame_zero(&[(1, LEFT), (2, RIGHT)], &[]));
        round(&mut watches, start, at, |_| Some(1)).unwrap();
        watches.hint(Hint::Focused(wid(2)), start);
        watches.watch(WatchId(1), changing(LEFT), start);
        let (_, outcome) = round(&mut watches, start, at, |w| Some(if w == wid(1) { 2 } else { 1 })).unwrap();
        assert_eq!(delivered(&outcome), vec![(Why::FocusChanged, vec![wid(1)])]);
        let next = start + CHANGE_EVERY;
        watches.retarget(WatchId(1), changing(rect(14.0, 32.0, 859.0, 1081.0)), next);
        assert_eq!(looked(&watches.next(next).unwrap()), vec![vec![wid(2)]], "the same size: still done");
        let wider = rect(4.0, 32.0, 860.0, 1081.0);
        watches.retarget(WatchId(1), changing(wider), next);
        let at = |w: WindowId| Some(if w == wid(1) { wider } else { RIGHT });
        let (sets, outcome) = round(&mut watches, next, at, |_| Some(2)).unwrap();
        assert_eq!(looked(&sets), vec![vec![wid(1), wid(2)]], "another size: watched afresh");
        assert_eq!(delivered(&outcome), vec![(Why::FocusChanged, vec![wid(2)])], "the target's baseline kept");
    }

    /// Windows behind the strip refresh every 16ms, every capture the cache took delivered whatever its
    /// band print: content changes below the band, which a band print would never see.
    #[test]
    fn the_background_refreshes_every_sixteen_milliseconds_and_delivers_each_capture_the_cache_took() {
        let start = Instant::now();
        let mut watches = Watches::default();
        watches.ask(frame_zero(&[], &[(4, LEFT)]));
        round(&mut watches, start, |_| Some(LEFT), |_| Some(1)).unwrap();
        watches.watch(WatchId(1), live(&[4], None, None), start);
        let (sets, outcome) = round(&mut watches, start, |_| Some(LEFT), |_| Some(1)).unwrap();
        assert!(sets[0].behind.is_empty());
        assert_eq!(
            delivered(&outcome),
            vec![(Why::Background, vec![wid(4)])],
            "unchanged, delivered"
        );
        assert!(watches.next(start + ms(15)).is_none());
        let (_, outcome) =
            round_of(&mut watches, start + ms(16), |_| Some(LEFT), |_| Capture::Refused).unwrap();
        assert!(outcome.deliveries.is_empty(), "refused: not delivered");
        let (_, outcome) = round(&mut watches, start + ms(32), |_| Some(LEFT), |_| Some(1)).unwrap();
        assert_eq!(delivered(&outcome), vec![(Why::Background, vec![wid(4)])]);
    }

    /// The background waits for what the transition waits on: a window still landing, and a change
    /// capture due within 8ms.
    #[test]
    fn the_background_never_runs_ahead_of_a_landing_or_a_change_capture() {
        let start = Instant::now();
        let mut watches = Watches::default();
        watches.watch(
            WatchId(1),
            landing(&[(1, LEFT)], Settle::AsSoonAsThere, start + ms(150)),
            start,
        );
        watches.watch(WatchId(2), live(&[4], None, None), start);
        let (sets, _) = round(
            &mut watches,
            start,
            |w| (w == wid(4)).then_some(RIGHT),
            |_| Some(1),
        )
        .unwrap();
        assert_eq!(
            looked(&sets),
            vec![vec![wid(1)]],
            "the landing, not the background"
        );
        assert!(
            watches.next(start).is_none(),
            "the background waits while it lands"
        );
        round(
            &mut watches,
            start + ms(1),
            |w| Some(if w == wid(1) { LEFT } else { RIGHT }),
            |_| Some(1),
        )
        .unwrap();
        assert_eq!(
            looked(&watches.next(start + ms(1)).unwrap()),
            vec![vec![wid(4)]],
            "landed: now"
        );

        let mut watches = Watches::default();
        watches.hint(Hint::Focused(wid(1)), start);
        watches.watch(WatchId(1), change(1, LEFT), start);
        watches.watch(WatchId(2), live(&[4], None, None), start);
        round(
            &mut watches,
            start,
            |w| Some(if w == wid(1) { LEFT } else { RIGHT }),
            |_| Some(1),
        )
        .unwrap();
        assert!(watches.next(start + ms(7)).is_none(), "the next try is 8ms off");
        assert_eq!(
            looked(&watches.next(start + ms(6)).unwrap()),
            vec![vec![wid(4)]],
            "9ms off"
        );
    }

    /// The background waits for the focus target's change watch to be done; once at `initial`
    /// regardless, for a window frame zero left out.
    #[test]
    fn the_background_waits_for_its_change_watch_but_once_at_initial() {
        let start = Instant::now();
        let mut watches = Watches::default();
        watches.ask(frame_zero(&[(1, LEFT)], &[]));
        round(&mut watches, start, |_| Some(LEFT), |_| Some(1)).unwrap();
        watches.watch(WatchId(1), change(1, LEFT), start);
        watches.watch(
            WatchId(2),
            live(&[4], Some(WatchId(1)), Some(start + ms(30))),
            start,
        );
        assert!(watches.next(start).is_none(), "the focus target first");
        assert_eq!(watches.wake(start), Some(start + ms(30)));
        let (sets, outcome) =
            round(&mut watches, start + ms(30), |_| Some(RIGHT), |_| Some(1)).unwrap();
        assert_eq!(looked(&sets), vec![vec![wid(4)]]);
        assert_eq!(delivered(&outcome), vec![(Why::Background, vec![wid(4)])]);
        assert!(watches.next(start + ms(100)).is_none(), "and then waits again");

        watches.hint(Hint::Focused(wid(1)), start + ms(100));
        let (_, outcome) = round(
            &mut watches,
            start + ms(100),
            |w| Some(if w == wid(1) { LEFT } else { RIGHT }),
            |_| Some(2),
        )
        .unwrap();
        assert_eq!(delivered(&outcome), vec![(Why::FocusChanged, vec![wid(1)])]);
        assert_eq!(
            looked(&watches.next(start + ms(100)).unwrap()),
            vec![vec![wid(4)]],
            "after the change"
        );
    }

    /// The background waits for the target's change, not for the window losing focus, which may go on
    /// alone for its six tries.
    #[test]
    fn the_background_waits_for_the_targets_change_and_not_the_old_windows() {
        let start = Instant::now();
        let at = |w: WindowId| Some(if w == wid(1) { LEFT } else { RIGHT });
        let mut watches = Watches::default();
        watches.ask(frame_zero(&[(1, LEFT), (2, RIGHT)], &[]));
        round(&mut watches, start, at, |_| Some(1)).unwrap();
        watches.hint(Hint::Focused(wid(2)), start);
        watches.watch(WatchId(1), changing(LEFT), start);
        watches.watch(WatchId(2), live(&[4], Some(WatchId(1)), None), start);
        let (sets, _) = round(&mut watches, start, at, |_| Some(1)).unwrap();
        assert_eq!(looked(&sets), vec![vec![wid(1), wid(2)]]);
        let quiet = start + ms(9);
        assert!(watches.next(quiet).is_none(), "the target's change to come");
        let next = start + CHANGE_EVERY;
        let (_, outcome) = round(&mut watches, next, at, |w| Some(if w == wid(2) { 2 } else { 1 })).unwrap();
        assert_eq!(delivered(&outcome), vec![(Why::FocusChanged, vec![wid(2)])]);
        assert_eq!(looked(&watches.next(next).unwrap()), vec![vec![wid(4)]], "the old window still watched");
    }

    /// The target's change in, the windows behind the strip keep their pace beside a window losing
    /// focus never where it ends, polled for every 1ms.
    #[test]
    fn the_background_keeps_its_pace_beside_an_old_window_never_where_it_ends() {
        let start = Instant::now();
        let short = rect(14.0, 32.0, 700.0, 1081.0);
        let at = |w: WindowId| Some(if w == wid(1) { short } else { RIGHT });
        let mut watches = Watches::default();
        watches.ask(frame_zero(&[(1, LEFT), (2, RIGHT)], &[]));
        round(&mut watches, start, |w| Some(if w == wid(1) { LEFT } else { RIGHT }), |_| Some(1)).unwrap();
        watches.hint(Hint::Focused(wid(2)), start);
        watches.watch(WatchId(1), changing(LEFT), start);
        watches.watch(WatchId(2), live(&[4], Some(WatchId(1)), None), start);
        let (_, outcome) = round(&mut watches, start, at, |w| Some(if w == wid(2) { 2 } else { 1 })).unwrap();
        assert_eq!(delivered(&outcome), vec![(Why::FocusChanged, vec![wid(2)])]);
        let (mut now, mut background, mut polls) = (start, Vec::new(), 0);
        while let Some(wake) = watches.wake(now)
            && wake < start + ms(50)
        {
            now = wake;
            let (sets, _) = round(&mut watches, now, at, |_| Some(1)).unwrap();
            match looked(&sets).concat().as_slice() {
                [window] if *window == wid(4) => background.push(now - start),
                _ => polls += 1,
            }
        }
        assert_eq!(background, vec![ms(0), LIVE_EVERY, LIVE_EVERY * 2, LIVE_EVERY * 3]);
        assert!(polls >= 45, "the old window still polled for: {polls}");
    }

    /// A poll looking at an end not there yet leaves the round where it was, even when its look ends
    /// past the round's time: the end captured before is looked at in that round.
    #[test]
    fn a_poll_finishing_past_the_round_leaves_the_round_where_it_was() {
        let start = Instant::now();
        let short = rect(700.0, 32.0, 859.0, 1081.0);
        let mut watches = Watches::default();
        watches.hint(Hint::Focused(wid(2)), start);
        watches.watch(WatchId(1), changing(LEFT), start);
        round(&mut watches, start, |w| Some(if w == wid(1) { LEFT } else { short }), |_| Some(1)).unwrap();
        let polled = start + CHANGE_EVERY - Duration::from_micros(400);
        assert_eq!(looked(&watches.next(polled).unwrap()), vec![vec![wid(2)]], "the poll alone");
        let seen = start + CHANGE_EVERY + Duration::from_micros(200);
        watches.seen(seen, &[Seen { window: wid(2), frame: Some(RIGHT), capture: taken(1) }], Some(LAPTOP));
        assert_eq!(looked(&watches.next(seen).unwrap()), vec![vec![wid(1), wid(2)]], "the round, both ends");
    }

    /// A frame written to either end not done brings the watch's round forward; to an end done, or
    /// to a window it does not watch, it does not.
    #[test]
    fn a_window_placed_brings_its_change_watchs_round_forward() {
        let start = Instant::now();
        let at = |w: WindowId| Some(if w == wid(1) { LEFT } else { RIGHT });
        let placed = |window: u32, done: bool| {
            let mut watches = Watches::default();
            watches.ask(frame_zero(&[(1, LEFT), (2, RIGHT)], &[]));
            round(&mut watches, start, at, |_| Some(1)).unwrap();
            watches.hint(Hint::Focused(wid(2)), start);
            watches.watch(WatchId(1), changing(LEFT), start);
            round(&mut watches, start, at, |w| Some(if done && w == wid(1) { 2 } else { 1 })).unwrap();
            let later = start + ms(2);
            assert!(watches.next(later).is_none());
            watches.hint(Hint::Placed(wid(window)), later);
            watches.next(later).map(|sets| looked(&sets))
        };
        assert_eq!(placed(1, false), Some(vec![vec![wid(1), wid(2)]]), "the old window placed");
        assert_eq!(placed(2, false), Some(vec![vec![wid(1), wid(2)]]));
        assert_eq!(placed(1, true), None, "its change seen");
        assert_eq!(placed(5, false), None);
    }

    /// A stopped change watch holds the background up no longer.
    #[test]
    fn the_background_follows_a_change_watch_that_was_stopped() {
        let start = Instant::now();
        let mut watches = Watches::default();
        watches.watch(WatchId(1), change(1, LEFT), start);
        watches.watch(WatchId(2), live(&[4], Some(WatchId(1)), None), start);
        assert!(watches.next(start).is_none());
        watches.stop(WatchId(1));
        assert!(watches.next(start).is_some());
    }

    /// The final ask takes the windows where they end, within 20pt, and delivers only what is news.
    #[test]
    fn the_final_ask_delivers_only_what_changed() {
        let start = Instant::now();
        let mut watches = Watches::default();
        watches.ask(frame_zero(&[(1, LEFT), (2, RIGHT)], &[]));
        round(
            &mut watches,
            start,
            |w| Some(if w == wid(1) { LEFT } else { RIGHT }),
            |_| Some(1),
        )
        .unwrap();
        watches.ask(OnScreen {
            moment: Moment::Final,
            ..frame_zero(&[(1, LEFT), (2, RIGHT)], &[])
        });
        let (_, outcome) = round(
            &mut watches,
            start,
            |w| Some(if w == wid(1) { LEFT } else { RIGHT }),
            |w| Some(if w == wid(1) { 1 } else { 2 }),
        )
        .unwrap();
        assert_eq!(delivered(&outcome), vec![(Why::Final, vec![wid(2)])]);

        watches.ask(OnScreen {
            moment: Moment::Final,
            ..frame_zero(&[(1, LEFT)], &[])
        });
        let (_, outcome) = round(&mut watches, start, |_| Some(LEFT), |_| Some(1)).unwrap();
        assert!(outcome.deliveries.is_empty(), "nothing new: no delivery at all");
    }

    /// Strip work due together is looked at in one round; work with different windows behind it is
    /// never in one call, since a window's blur is of what is composited after it.
    #[test]
    fn strip_work_due_together_shares_a_call_only_with_the_same_windows_behind() {
        let start = Instant::now();
        let mut watches = Watches::default();
        watches.hint(Hint::Focused(wid(1)), start);
        watches.watch(WatchId(1), change(1, LEFT), start);
        watches.watch(
            WatchId(2),
            landing(&[(1, LEFT), (2, RIGHT)], Settle::AsSoonAsThere, start + ms(150)),
            start,
        );
        watches.watch(
            WatchId(3),
            landing_of(
                tag(1),
                Vec::new(),
                &[(3, rect(100.0, 100.0, 400.0, 300.0))],
                Settle::AsSoonAsThere,
                start + ms(150),
            ),
            start,
        );
        let sets = watches.next(start).unwrap();
        assert_eq!(
            looked(&sets),
            vec![vec![wid(1), wid(2)], vec![wid(3)]],
            "window 1 looked at once"
        );
    }

    /// A merged press retargets: a window that landed where it still ends stays landed, one with a new
    /// end and one new to the watch are watched from now; a focus seen stays seen.
    #[test]
    fn a_retarget_keeps_what_still_holds_and_watches_what_is_new() {
        let start = Instant::now();
        let mut watches = Watches::default();
        watches.watch(
            WatchId(1),
            landing(&[(1, LEFT), (2, RIGHT)], Settle::AsSoonAsThere, start + ms(150)),
            start,
        );
        round(
            &mut watches,
            start,
            |w| Some(if w == wid(1) { LEFT } else { RIGHT }),
            |_| Some(1),
        )
        .unwrap();
        let moved = rect(865.0, 32.0, 430.0, 1081.0);
        watches.retarget(
            WatchId(1),
            landing_of(
                tag(2),
                vec![WindowServerId::new(9)],
                &[
                    (1, LEFT),
                    (2, moved),
                    (3, rect(1300.0, 32.0, 400.0, 1081.0)),
                ],
                Settle::AsSoonAsThere,
                start + ms(300),
            ),
            start + ms(50),
        );
        let sets = watches.next(start + ms(50)).unwrap();
        assert_eq!(looked(&sets), vec![vec![wid(2), wid(3)]]);

        let mut watches = Watches::default();
        watches.hint(Hint::Focused(wid(1)), start);
        watches.watch(WatchId(1), change(1, LEFT), start);
        watches.retarget(WatchId(1), change(1, RIGHT), start + ms(2000));
        assert!(
            watches.next(start + ms(2000)).is_some(),
            "armed still, long after the report"
        );
        watches.retarget(WatchId(1), change(2, RIGHT), start + ms(2000));
        assert!(
            watches.next(start + ms(2000)).is_none(),
            "another window: not armed"
        );
    }

    /// A window that landed and is retargeted to a new end settles there only on two captures in a
    /// row that match: the server resizes the frame before the app repaints, so the first capture
    /// there still shows the picture it landed with, and the next can be half-painted.
    #[test]
    fn a_window_that_landed_and_is_retargeted_to_a_new_end_settles_there_on_two_captures_in_a_row() {
        let start = Instant::now();
        let mut watches = Watches::default();
        watches.watch(
            WatchId(1),
            landing(&[(1, LEFT)], Settle::Repainted, start + ms(150)),
            start,
        );
        round(&mut watches, start, |_| Some(LEFT), |_| Some(2)).unwrap();
        let (_, outcome) =
            round(&mut watches, start + REPAINT_EVERY, |_| Some(LEFT), |_| Some(2)).unwrap();
        assert_eq!(delivered(&outcome), vec![(Why::Landed, vec![wid(1)])]);
        let moved = rect(4.0, 32.0, 1200.0, 1081.0);
        let retargeted = start + REPAINT_EVERY + ms(1);
        watches.retarget(
            WatchId(1),
            landing(&[(1, moved)], Settle::Repainted, start + ms(150)),
            retargeted,
        );
        let mut at = retargeted;
        for (band, why) in [
            (2, "the picture it landed with"),
            (3, "half-painted"),
            (4, "painted, once"),
        ] {
            let (_, outcome) = round(&mut watches, at, |_| Some(moved), |_| Some(band)).unwrap();
            assert!(outcome.deliveries.is_empty(), "{why}: not settled");
            at += REPAINT_EVERY;
        }
        let (_, outcome) = round(&mut watches, at, |_| Some(moved), |_| Some(4)).unwrap();
        assert_eq!(delivered(&outcome), vec![(Why::Landed, vec![wid(1)])], "painted twice in a row");
    }

    /// A window's landing delivered at one end, and its watch retargeted to another: the client draws
    /// nothing taken for the end before, so the window is delivered where it lands now, looking the
    /// same or not. One still ending where it landed is not delivered again.
    #[test]
    fn a_window_watched_to_land_somewhere_new_is_delivered_there_whatever_it_looks_like() {
        let start = Instant::now();
        let mut watches = Watches::default();
        watches.watch(
            WatchId(1),
            landing(&[(1, LEFT), (2, RIGHT)], Settle::AsSoonAsThere, start + ms(150)),
            start,
        );
        let at = |w: WindowId| Some(if w == wid(1) { LEFT } else { RIGHT });
        round(&mut watches, start, at, |_| Some(1)).unwrap();
        let moved = rect(1000.0, 32.0, 700.0, 1081.0);
        watches.retarget(
            WatchId(1),
            landing_of(
                tag(2),
                vec![WindowServerId::new(9)],
                &[(1, moved), (2, RIGHT)],
                Settle::AsSoonAsThere,
                start + ms(300),
            ),
            start + ms(50),
        );
        let (sets, outcome) = round(&mut watches, start + ms(50), |_| Some(moved), |_| Some(1)).unwrap();
        assert_eq!(looked(&sets), vec![vec![wid(1)]]);
        assert_eq!(delivered(&outcome), vec![(Why::Landed, vec![wid(1)])]);
    }

    /// Deliveries carry the tag of the watch as it is now, so a client tells its generations apart.
    #[test]
    fn a_retargeted_watch_delivers_under_its_new_tag() {
        let start = Instant::now();
        let mut watches = Watches::default();
        watches.watch(
            WatchId(1),
            landing(&[(1, LEFT)], Settle::AsSoonAsThere, start + ms(150)),
            start,
        );
        watches.retarget(
            WatchId(1),
            landing_of(
                tag(2),
                vec![WindowServerId::new(9)],
                &[(1, LEFT)],
                Settle::AsSoonAsThere,
                start + ms(150),
            ),
            start,
        );
        let (_, outcome) = round(&mut watches, start, |_| Some(LEFT), |_| Some(1)).unwrap();
        assert_eq!(outcome.deliveries[0].tag, tag(2));
    }

    /// A window that is gone is asked for and watched no more, and its prints and focus go too.
    #[test]
    fn a_forgotten_window_is_watched_no_more() {
        let start = Instant::now();
        let mut watches = Watches::default();
        watches.hint(Hint::Focused(wid(1)), start);
        watches.watch(
            WatchId(1),
            landing(&[(1, LEFT), (2, RIGHT)], Settle::AsSoonAsThere, start + ms(150)),
            start,
        );
        watches.ask(frame_zero(&[(1, LEFT)], &[]));
        watches.forget(wid(1));
        let (sets, outcome) = round(&mut watches, start, |_| Some(LEFT), |_| Some(1)).unwrap();
        assert_eq!(looked(&sets), vec![vec![]], "frame zero without it");
        assert_eq!(delivered(&outcome), vec![(Why::FrameZero, vec![])]);
        assert_eq!(looked(&watches.next(start).unwrap()), vec![vec![wid(2)]]);
        watches.stop(WatchId(1));
        watches.watch(WatchId(2), change(1, LEFT), start);
        assert!(watches.next(start).is_none(), "its focus report forgotten");
    }

    #[test]
    fn a_window_is_on_the_display_only_wholly_and_clipped_when_cut_by_its_edge() {
        assert!(wholly_on(LEFT, LAPTOP));
        assert!(
            wholly_on(rect(-1.0, 0.0, 1729.0, 1117.0), LAPTOP),
            "a point of slack"
        );
        assert!(!wholly_on(PARKED, LAPTOP));
        assert!(clipped(rect(1300.0, 32.0, 859.0, 1081.0), LAPTOP));
        assert!(!clipped(LEFT, LAPTOP), "wholly on it");
        assert!(
            !clipped(rect(2000.0, 32.0, 859.0, 1081.0), LAPTOP),
            "wholly off it"
        );
    }

    /// See "A grow waits for its repaint" in `src/transitions/docs/coordination.md`.
    #[test]
    fn a_window_that_has_to_repaint_is_captured_every_8ms_for_a_second() {
        assert_eq!(REPAINT_EVERY, Duration::from_millis(8));
        assert_eq!(REPAINT_EVERY * REPAINT_TRIES as u32, Duration::from_secs(1));
    }
}
