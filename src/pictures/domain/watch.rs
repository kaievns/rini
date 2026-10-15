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
//!
//! This file is the asks, the types every watch shares and `Watches`, which hands each watch's work
//! to its kind: `watch/landing.rs`, `watch/change.rs` and `watch/live.rs`.

mod change;
#[cfg(test)]
mod fixtures;
mod landing;
mod live;

use std::collections::HashMap;
use std::time::{Duration, Instant};

use rini_core::ids::{WindowId, WindowServerId};
use rini_geometry::space::{ScreenPoint, ScreenRect};
use serde::Serialize;

use self::change::{Change, Turning};
use self::landing::Landing;
use self::live::Live;
use crate::pictures::domain::print::print_changed;
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
    prints: Delivered,
    /// The tag whose frame zero each client's prints were last started afresh by.
    started: HashMap<Client, Tag>,
    plan: Plan,
    /// Windows losing focus a retarget carried on given up on, to report lost under its new tag.
    lost: Vec<(Tag, WindowId)>,
}

/// The band print each client was last delivered of each window.
type Delivered = HashMap<(Client, WindowId), Vec<u8>>;

/// What the round handed out was for.
#[derive(Default)]
struct Plan {
    asks: Vec<OnScreen>,
    due: Vec<(WatchId, WindowId)>,
    /// The change watches looked at for their round, not only polled.
    rounds: Vec<WatchId>,
}

/// A round being handed out: the sets to capture, and what each look in them is for.
#[derive(Default)]
struct Round {
    sets: Vec<Set>,
    plan: Plan,
}

impl Round {
    /// `look`, for watch `id`, in the set listing exactly `behind`.
    fn add(&mut self, id: WatchId, behind: &[WindowServerId], look: Look) {
        self.plan.due.push((id, look.target.window));
        add(&mut self.sets, behind, look);
    }
}

/// What the window server and the captures said of the windows a round looked at, on `display`.
struct Looked<'a> {
    seen: &'a [Seen],
    display: Option<ScreenRect>,
}

impl<'a> Looked<'a> {
    fn of(&self, window: WindowId) -> Option<&'a Seen> {
        self.seen.iter().find(|seen| seen.window == window)
    }

    /// The window's server frame, where `accept` takes it, and what its capture came to.
    fn at(&self, window: WindowId, accept: Accept) -> Option<(ScreenRect, &'a Capture)> {
        let seen = self.of(window)?;
        let frame = seen.frame?;
        self.display
            .is_some_and(|display| accept.accepts(frame, display))
            .then_some((frame, &seen.capture))
    }

    /// The window's server frame and prints, where `accept` takes it and the cache took its capture.
    fn taken(&self, window: WindowId, accept: Accept) -> Option<(ScreenRect, &'a Prints)> {
        match self.at(window, accept)? {
            (frame, Capture::Taken(prints)) => Some((frame, prints)),
            (_, Capture::Missed | Capture::Refused) => None,
        }
    }

    /// The window's server frame, where it is within `slack` of `end` but cut by the display's edge.
    fn cut(&self, window: WindowId, end: ScreenRect, slack: f64) -> Option<ScreenRect> {
        let frame = self.of(window)?.frame?;
        let display = self.display?;
        (near(frame, end, slack) && clipped(frame, display)).then_some(frame)
    }
}

enum Watching {
    Landing(Landing),
    Change(Change),
    Live(Live),
}

impl Watches {
    pub fn ask(&mut self, ask: OnScreen) { self.asks.push(ask) }

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
            (Watching::Landing(new), Watching::Landing(old)) => new.carry_on(old, &mut self.prints),
            (Watching::Change(new), Watching::Change(old)) => self.lost.extend(new.carry_on(old)),
            (Watching::Live(new), Watching::Live(old)) => new.carry_on(old),
            _ => {}
        }
    }

    pub fn stop(&mut self, id: WatchId) { self.watches.retain(|(held, _)| *held != id) }

    pub fn hint(&mut self, hint: Hint, now: Instant) {
        let Hint::Focused(window) = hint;
        self.focused.retain(|(held, at)| {
            *held != window && now.saturating_duration_since(*at) <= FOCUS_REMEMBERED
        });
        self.focused.push((window, now));
        for (_, watching) in &mut self.watches {
            if let Watching::Change(change) = watching {
                change.arm(window, now);
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
                landing.forget(window);
                true
            }
            Watching::Change(change) => change.forget(window),
            Watching::Live(live) => {
                live.forget(window);
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
            if let Watching::Landing(landing) = watching {
                lost.extend(landing.expire(now));
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

        let mut round = Round::default();
        for (id, watching) in &self.watches {
            match watching {
                Watching::Landing(landing) => landing.due_looks(*id, now, &mut round),
                Watching::Change(change) => change.due_looks(*id, now, &mut round),
                Watching::Live(_) => {}
            }
        }
        let (finals, rest) = std::mem::take(&mut self.asks)
            .into_iter()
            .partition(|ask| ask.moment == Moment::Final);
        self.asks = rest;
        for ask in &finals {
            for set in ask_sets(ask) {
                for look in set.looks {
                    add(&mut round.sets, &set.behind, look);
                }
            }
        }
        if round.sets.is_empty() {
            for (id, watching) in &self.watches {
                if let Watching::Live(live) = watching
                    && self.live_due(live, now).is_some_and(|at| at <= now)
                {
                    live.due_looks(*id, &mut round);
                }
            }
        } else {
            round.plan.asks = finals;
        }
        if round.sets.is_empty() {
            return None;
        }
        self.plan = round.plan;
        Some(round.sets)
    }

    /// When something is next due, if anything is.
    pub fn wake(&self, now: Instant) -> Option<Instant> {
        let asked = (!self.asks.is_empty() || !self.lost.is_empty()).then_some(now);
        let watched = self.watches.iter().filter_map(|(_, watching)| match watching {
            Watching::Landing(landing) => landing.wake(),
            Watching::Change(change) => change.wake(),
            Watching::Live(live) => self.live_due(live, now),
        });
        asked.into_iter().chain(watched).min()
    }

    /// What the window server and the captures said of the windows `next` handed out.
    pub fn seen(&mut self, now: Instant, seen: &[Seen], display: Option<ScreenRect>) -> Outcome {
        let plan = std::mem::take(&mut self.plan);
        let looked = Looked { seen, display };
        let mut out = Outcome::default();
        for ask in plan.asks {
            self.answer(ask, &looked, &mut out);
        }
        // Each change watch looked at, and whether this look captured any of its ends.
        let mut changed: Vec<(WatchId, bool)> = Vec::new();
        for (id, window) in plan.due {
            let Some((_, watching)) = self.watches.iter_mut().find(|(held, _)| *held == id) else {
                continue;
            };
            let prints = &mut self.prints;
            match watching {
                Watching::Landing(landing) => {
                    landing.on_seen(window, now, &looked, prints, &mut out)
                }
                Watching::Change(change) => {
                    let Some(captured) = change.on_seen(window, &looked, prints, &mut out) else {
                        continue;
                    };
                    match changed.iter_mut().find(|(held, _)| *held == id) {
                        Some((_, any)) => *any |= captured,
                        None => changed.push((id, captured)),
                    }
                }
                Watching::Live(live) => live.on_seen(window, now, &looked, prints, &mut out),
            }
        }
        for (id, captured) in changed {
            if let Some((_, Watching::Change(change))) =
                self.watches.iter_mut().find(|(held, _)| *held == id)
            {
                change.reschedule(now, plan.rounds.contains(&id), captured);
            }
        }
        out
    }

    /// What the ScreenCaptureKit capture `window` was owed, settling where it ends cut by the
    /// display's edge, came to: delivered as landed, at the frame it was found there, once its
    /// thumbprint matches the capture's before it; lost when the cache refuses it or its tries run
    /// out; else owed another. Its band print is not taken from a surface, so the client's print of it
    /// is dropped as it is delivered.
    pub fn captured_elsewhere(&mut self, window: WindowId, capture: CapturedElsewhere) -> Outcome {
        let mut out = Outcome::default();
        for (_, watching) in &mut self.watches {
            if let Watching::Landing(landing) = watching {
                landing.captured_elsewhere(window, &capture, &mut self.prints, &mut out);
            }
        }
        out
    }

    /// What the round saw of `ask`'s windows: frame zero delivers every window it took, in one
    /// delivery even with none, and the final ask only the news.
    fn answer(&mut self, ask: OnScreen, looked: &Looked, out: &mut Outcome) {
        let client = ask.tag.client;
        // A transition's frame zero asked again, for what a merge brought on screen, adds to it.
        if ask.moment == Moment::FrameZero && self.started.insert(client, ask.tag) != Some(ask.tag)
        {
            self.prints.retain(|(held, _), _| *held != client);
        }
        let mut delivered = Vec::new();
        for (target, expected) in ask.strip.iter().chain(&ask.background) {
            let accept = accept_for(ask.moment, *expected);
            let Some((frame, prints)) = looked.taken(target.window, accept) else {
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

    fn start(&self, watch: Watch, now: Instant) -> Watching {
        match watch {
            Watch::Landing {
                tag,
                windows,
                behind,
                slack,
                settle,
                until,
            } => Watching::Landing(Landing::start(tag, windows, behind, slack, settle, until, now)),
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
                let (target, losing) = (turning((window, end)), losing.map(turning));
                Watching::Change(Change::start(tag, target, losing, behind, every, armed, now))
            }
            Watch::Live {
                tag,
                windows,
                every,
                after,
                initial,
            } => Watching::Live(Live::start(tag, windows, every, after, initial)),
        }
    }

    /// When `live` is next due, unless something it never runs ahead of is pending: a window still
    /// landing, or a change watch's target capture due within `LIVE_YIELDS_WITHIN`. The window losing
    /// focus does not: one never where it ends is polled for every `LANDING_POLL` to the flight's end.
    fn live_due(&self, live: &Live, now: Instant) -> Option<Instant> {
        let held = self.watches.iter().any(|(_, watching)| match watching {
            Watching::Landing(landing) => landing.waiting(),
            Watching::Change(change) => change.target_due_by(now + LIVE_YIELDS_WITHIN),
            Watching::Live(_) => false,
        });
        if held {
            return None;
        }
        live.due(now, self.after_done(live.after()))
    }

    /// Whether the watch `after` names has delivered its target's change, run out of it, or gone:
    /// the window losing focus may go on alone.
    fn after_done(&self, after: Option<WatchId>) -> bool {
        let Some(after) = after else { return true };
        self.watches.iter().all(|(id, watching)| {
            *id != after || !matches!(watching, Watching::Change(change) if !change.target_done())
        })
    }
}

/// Whether `band` is news to `client` of `window`, which it is then told of.
fn news(prints: &mut Delivered, client: Client, window: WindowId, band: &[u8]) -> bool {
    let new = print_changed(prints.get(&(client, window)).map(Vec::as_slice), band);
    prints.insert((client, window), band.to_vec());
    new
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
    use super::fixtures::*;
    use super::*;

    /// Frame zero names windows where their tiles start: the strip in one set with what is behind it,
    /// the background in its own, never one composite. Whatever is there within 2pt and wholly on the
    /// display is delivered, all in one delivery.
    #[test]
    fn frame_zero_delivers_every_window_where_it_starts_in_one_delivery() {
        let mut watches = Watches::default();
        watches.ask(frame_zero(&[(1, LEFT), (2, RIGHT), (3, PARKED)], &[(
            4,
            rect(100.0, 100.0, 400.0, 300.0),
        )]));
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

    /// What a round saw of a window counts only where `accept` takes its server frame on a known
    /// display, and as taken only when the cache took its capture; it is cut only within `slack` of
    /// its end and partly on the display.
    #[test]
    fn a_window_looked_at_counts_only_where_accepted_and_is_cut_only_near_its_end() {
        let cut = rect(1300.0, 32.0, 859.0, 1081.0);
        let seen = |window, frame, capture| Seen { window, frame, capture };
        let seen = [
            seen(wid(1), Some(LEFT), taken(1)),
            seen(wid(2), Some(RIGHT), Capture::Refused),
            seen(wid(3), None, taken(1)),
            seen(wid(4), Some(cut), Capture::Missed),
        ];
        let looked = Looked {
            seen: &seen,
            display: Some(LAPTOP),
        };
        let near = |end| Accept::Near { end, slack: END_SLACK };
        assert_eq!(
            looked.taken(wid(1), near(LEFT)).map(|(frame, _)| frame),
            Some(LEFT)
        );
        assert!(
            looked.taken(wid(1), near(RIGHT)).is_none(),
            "not where it is accepted"
        );
        assert_eq!(
            looked.at(wid(2), Accept::OnDisplay),
            Some((RIGHT, &Capture::Refused))
        );
        assert!(looked.taken(wid(2), Accept::OnDisplay).is_none(), "refused");
        assert!(looked.at(wid(3), Accept::OnDisplay).is_none(), "no server frame");
        assert!(looked.at(wid(5), Accept::OnDisplay).is_none(), "not looked at");
        assert_eq!(looked.cut(wid(4), cut, END_SLACK), Some(cut));
        assert!(looked.cut(wid(4), LEFT, END_SLACK).is_none(), "far from its end");
        assert!(
            looked.cut(wid(1), LEFT, END_SLACK).is_none(),
            "wholly on the display"
        );
        let unknown = Looked { seen: &seen, display: None };
        assert!(unknown.at(wid(1), near(LEFT)).is_none(), "no display known");
        assert!(unknown.cut(wid(4), cut, END_SLACK).is_none(), "no display known");
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
}
