//! Captures of windows on screen, through SkyLight: what a transition asks for at frame zero and just
//! before its lift, and the watches that capture a window as it lands where it ends, as its look
//! changes after a focus report, and as the windows behind the strip repaint.
//!
//! Pure. Frames and prints come in with `now`; the windows to look at, and what to deliver to whom, go
//! out. `crate::pictures::domain::schedule` orders this work against ScreenCaptureKit's, and
//! `crate::pictures::platform::service` reads the frames, makes the calls and prints the pictures. The
//! rules and why each holds: "The watches" in `src/pictures/docs/README.md`.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use objc2_core_foundation::{CGPoint, CGRect};
use rini_core::ids::{WindowId, WindowServerId};
use rini_geometry::CGRectExt;

use crate::pictures::domain::print::print_changed;
use crate::pictures::domain::schedule::{REVEAL_CHASE_ATTEMPTS, REVEAL_CHASE_INTERVAL};
use crate::pictures::domain::settle::chase_settled;
use crate::pictures::domain::target::PictureTarget;

/// A window whose server frame is this close to where it ends, in each of x, y, width and height,
/// has landed: the window server rounds each window on its own, and an app may settle a point or two
/// off what it was asked. The same slack a transition draws a picture on its tile with.
pub const END_SLACK: f64 = 20.0;
/// A window this close to where its tile starts is where frame zero expects it.
const FRAME_ZERO_SLACK: f64 = 2.0;
/// A frame this far past a display's edge still counts as on it.
const ON_DISPLAY_SLACK: f64 = 1.0;
/// How often a window not yet landed has its server frame read.
pub const LANDING_POLL: Duration = Duration::from_millis(1);
/// How often, and how many times, a window reported focused is captured until its look changes.
pub const CHANGE_EVERY: Duration = Duration::from_millis(15);
pub const CHANGE_TRIES: u32 = 6;
/// The least time between two refreshes of the windows behind the strip.
pub const LIVE_EVERY: Duration = Duration::from_millis(16);
/// A refresh behind the strip waits for a change capture due this soon.
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
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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
    pub strip: Vec<(PictureTarget, CGRect)>,
    /// Windows behind the strip that stay put. Captured in calls of their own: in one composite with
    /// the strip, a translucent window would bake what is behind it into its picture.
    pub background: Vec<(PictureTarget, CGRect)>,
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
    /// The first capture whose rendering has settled, by `chase_settled`: for a window a transition
    /// holds its flight for, which grows or opens and is not painted at its new size yet. Captured every
    /// `REVEAL_CHASE_INTERVAL`, `REVEAL_CHASE_ATTEMPTS` times at most once there.
    Repainted,
}

#[derive(Clone, Debug)]
pub enum Watch {
    /// Each window captured once the window server has it within `slack` of where it ends and wholly on
    /// the display; one not there by `until` is reported lost.
    Landing {
        tag: Tag,
        windows: Vec<(PictureTarget, CGRect)>,
        behind: Vec<WindowServerId>,
        slack: f64,
        settle: Settle,
        until: Instant,
    },
    /// Armed by the window's focus report: captured where it ends every `every`, `tries` times at most,
    /// until its band print changes from the last picture delivered of it.
    Change {
        tag: Tag,
        window: PictureTarget,
        end: CGRect,
        behind: Vec<WindowServerId>,
        every: Duration,
        tries: u32,
    },
    /// Windows behind the strip, refreshed every `every`, never ahead of a landing or a change capture,
    /// and only once the `after` watch is done; but once at `initial` whatever `after` says.
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
    Near { end: CGRect, slack: f64 },
    /// With its origin within `FRAME_ZERO_SLACK` of `at`, and wholly on the display.
    Origin { at: CGPoint },
    /// Wholly on the display, wherever.
    OnDisplay,
}

impl Accept {
    pub fn accepts(&self, frame: CGRect, display: CGRect) -> bool {
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
pub fn wholly_on(frame: CGRect, display: CGRect) -> bool {
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
fn clipped(frame: CGRect, display: CGRect) -> bool {
    frame.intersection(&display).area() > 0.0 && !wholly_on(frame, display)
}

fn near(a: CGRect, b: CGRect, slack: f64) -> bool {
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
    pub frame: Option<CGRect>,
    pub capture: Capture,
}

/// What came of capturing a window looked at. A watch decides only on pictures the cache took, so
/// what it delivers, and the prints it judges news by, are what its client was given.
#[derive(Clone, Debug, PartialEq)]
pub enum Capture {
    /// No picture: the window was not where it was wanted, or the call brought none back.
    Missed,
    /// A picture the cache refused for the one it holds.
    Refused,
    /// A picture the cache took, with its prints.
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
    pub windows: Vec<(WindowId, CGRect)>,
}

/// What a round of looking decided.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Outcome {
    pub deliveries: Vec<Delivery>,
    /// Windows a landing watch gave up on.
    pub lost: Vec<(Tag, WindowId)>,
    /// A changing window cut by the display's edge, which SkyLight cannot picture whole: one capture of
    /// it is owed through ScreenCaptureKit, delivered as its focus change.
    pub clipped: Vec<(Tag, PictureTarget, CGRect)>,
}

impl Outcome {
    fn deliver(&mut self, tag: Tag, why: Why, window: WindowId, frame: CGRect) {
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
    plan: Plan,
}

/// What the round handed out was for.
#[derive(Default)]
struct Plan {
    asks: Vec<OnScreen>,
    due: Vec<(WatchId, WindowId)>,
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
    end: CGRect,
    /// The thumbprint of the picture held before the window changed, for `Settle::Repainted`.
    before: Option<Vec<u8>>,
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
    Arrived,
    Lost,
}

struct Change {
    tag: Tag,
    target: PictureTarget,
    end: CGRect,
    behind: Vec<WindowServerId>,
    every: Duration,
    left: u32,
    armed: bool,
    next: Instant,
    /// Its look changed, its tries ran out, or it was handed to ScreenCaptureKit.
    done: bool,
}

impl Change {
    /// A try spent: the next is `every` from now, and with none left the watch is done.
    fn spend(&mut self, now: Instant) {
        self.left = self.left.saturating_sub(1);
        self.done = self.left == 0;
        self.next = now + self.every;
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

    /// Starts `watch` as `id`. `before` is the thumbprint of the picture held of a window, which a
    /// repainting landing settles against.
    pub fn watch(
        &mut self,
        id: WatchId,
        watch: Watch,
        now: Instant,
        before: &dyn Fn(WindowId) -> Option<Vec<u8>>,
    ) {
        self.watches.retain(|(held, _)| *held != id);
        let watching = self.start(watch, now, before);
        self.watches.push((id, watching));
    }

    /// Replaces what `id` watches, keeping what it already knows: a window that landed where it still
    /// ends stays landed, a repainting window settles against the picture held when the watch first
    /// named it, a focus already seen stays seen, and the background keeps its pace.
    pub fn retarget(
        &mut self,
        id: WatchId,
        watch: Watch,
        now: Instant,
        before: &dyn Fn(WindowId) -> Option<Vec<u8>>,
    ) {
        let Some(at) = self.watches.iter().position(|(held, _)| *held == id) else {
            return self.watch(id, watch, now, before);
        };
        let fresh = self.start(watch, now, before);
        let old = std::mem::replace(&mut self.watches[at].1, fresh);
        match (&mut self.watches[at].1, old) {
            (Watching::Landing(new), Watching::Landing(old)) => {
                for lands in &mut new.windows {
                    let Some(kept) =
                        old.windows.iter().find(|o| o.target.window == lands.target.window)
                    else {
                        continue;
                    };
                    // What the cache holds now is the watch's own capture, not the picture before.
                    lands.before = kept.before.clone().or(lands.before.take());
                    if kept.end == lands.end && kept.state == Arrival::Arrived {
                        lands.state = Arrival::Arrived;
                    }
                }
            }
            (Watching::Change(new), Watching::Change(old))
                if new.target.window == old.target.window =>
            {
                new.armed = old.armed;
                new.left = old.left.min(new.left);
                new.next = old.next;
                new.done = old.done;
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
                        && change.target.window == window
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
                            if change.target.window == window && change.armed && !change.done =>
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
            Watching::Change(change) => change.target.window != window,
            Watching::Live(live) => {
                live.targets.retain(|target| target.window != window);
                true
            }
        });
        self.focused.retain(|(held, _)| *held != window);
        self.prints.retain(|(_, held), _| *held != window);
    }

    /// Windows a landing watch waited for until its deadline, each reported once.
    pub fn expire(&mut self, now: Instant) -> Vec<(Tag, WindowId)> {
        let mut lost = Vec::new();
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
                due: Vec::new(),
            };
            return Some(sets);
        }

        let mut sets: Vec<Set> = Vec::new();
        let mut due = Vec::new();
        for (id, watching) in &self.watches {
            match watching {
                Watching::Change(change) if change.armed && !change.done && change.next <= now => {
                    add(
                        &mut sets,
                        &change.behind,
                        look(
                            change.target,
                            Accept::Near {
                                end: change.end,
                                slack: END_SLACK,
                            },
                            false,
                        ),
                    );
                    due.push((*id, change.target.window));
                }
                Watching::Landing(landing) => {
                    for lands in &landing.windows {
                        let wanted = match &lands.state {
                            Arrival::Pending => landing.poll <= now,
                            Arrival::Settling { next, .. } => *next <= now,
                            Arrival::Arrived | Arrival::Lost => false,
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
            self.plan = Plan { asks: finals, due };
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
        self.plan = Plan { asks: Vec::new(), due };
        Some(vec![background])
    }

    /// When something is next due, if anything is.
    pub fn wake(&self, now: Instant) -> Option<Instant> {
        let mut at: Option<Instant> = (!self.asks.is_empty()).then_some(now);
        let mut take = |t: Instant| at = Some(at.map_or(t, |at| at.min(t)));
        for (_, watching) in &self.watches {
            match watching {
                Watching::Change(change) if change.armed && !change.done => take(change.next),
                Watching::Landing(landing) => {
                    for lands in &landing.windows {
                        match &lands.state {
                            Arrival::Pending => {
                                take(landing.poll);
                                take(landing.until);
                            }
                            Arrival::Settling { next, .. } => take(*next),
                            Arrival::Arrived | Arrival::Lost => {}
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
    pub fn seen(&mut self, now: Instant, seen: &[Seen], display: Option<CGRect>) -> Outcome {
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
            if ask.moment == Moment::FrameZero {
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
                        let there = looked_at(window, Accept::Near { end: lands.end, slack });
                        land(lands, settle, tag, there, now, &mut self.prints, &mut out);
                    }
                    if landing.windows.iter().any(|l| l.state == Arrival::Pending) {
                        landing.poll = now + LANDING_POLL;
                    }
                }
                Watching::Change(change) => {
                    let accept = Accept::Near {
                        end: change.end,
                        slack: END_SLACK,
                    };
                    match looked_at(window, accept) {
                        Some((frame, Capture::Taken(prints))) => {
                            let known = self.prints.contains_key(&(change.tag.client, window));
                            if news(&mut self.prints, change.tag.client, window, &prints.band)
                                && known
                            {
                                change.done = true;
                                out.deliver(change.tag, Why::FocusChanged, window, frame);
                            } else {
                                // With nothing delivered to judge a change by, the first capture is
                                // delivered as the window where it ends, and the change judged from it.
                                if !known {
                                    out.deliver(change.tag, Why::Landed, window, frame);
                                }
                                change.spend(now);
                            }
                        }
                        Some((_, Capture::Refused)) => change.spend(now),
                        Some((_, Capture::Missed)) | None => {
                            let frame = of(window).and_then(|seen| seen.frame);
                            if let (Some(frame), Some(display)) = (frame, display)
                                && near(frame, change.end, END_SLACK)
                                && clipped(frame, display)
                            {
                                change.done = true;
                                out.clipped.push((change.tag, change.target, frame));
                            } else {
                                change.next = now + LANDING_POLL;
                            }
                        }
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
        out
    }

    fn start(
        &self,
        watch: Watch,
        now: Instant,
        before: &dyn Fn(WindowId) -> Option<Vec<u8>>,
    ) -> Watching {
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
                        before: (settle == Settle::Repainted)
                            .then(|| before(target.window))
                            .flatten(),
                        state: Arrival::Pending,
                    })
                    .collect(),
                poll: now,
            }),
            Watch::Change {
                tag,
                window,
                end,
                behind,
                every,
                tries,
            } => {
                let armed = self.focused.iter().any(|(held, at)| {
                    *held == window.window && now.saturating_duration_since(*at) <= FOCUS_REMEMBERED
                });
                Watching::Change(Change {
                    tag,
                    target: window,
                    end,
                    behind,
                    every,
                    left: tries,
                    armed,
                    next: now,
                    done: tries == 0,
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
    /// landing, or a change capture due within `LIVE_YIELDS_WITHIN`.
    fn live_due(&self, live: &Live, now: Instant) -> Option<Instant> {
        let landing = self.watches.iter().any(|(_, watching)| {
            matches!(watching, Watching::Landing(landing)
                if landing.windows.iter().any(|l| matches!(l.state, Arrival::Pending | Arrival::Settling { .. })))
        });
        let change_soon = self.watches.iter().any(|(_, watching)| {
            matches!(watching, Watching::Change(change)
                if change.armed && !change.done && change.next <= now + LIVE_YIELDS_WITHIN)
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

    /// Whether the watch `after` names has delivered its change, run out, or gone.
    fn after_done(&self, after: Option<WatchId>) -> bool {
        let Some(after) = after else { return true };
        self.watches.iter().all(|(id, watching)| {
            *id != after || !matches!(watching, Watching::Change(change) if !change.done)
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
    there: Option<(CGRect, &Capture)>,
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
        done @ (Arrival::Arrived | Arrival::Lost) => {
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
        Arrival::Pending => (REVEAL_CHASE_ATTEMPTS, None),
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
        .is_some_and(|thumb| chase_settled(last.as_deref(), thumb, lands.before.as_deref()));
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
                next: now + REVEAL_CHASE_INTERVAL,
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

fn accept_for(moment: Moment, expected: CGRect) -> Accept {
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
    let looks = |windows: &[(PictureTarget, CGRect)]| {
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
    use objc2_core_foundation::CGSize;

    use super::*;

    const LAPTOP: CGRect = CGRect {
        origin: CGPoint { x: 0.0, y: 0.0 },
        size: CGSize { width: 1728.0, height: 1117.0 },
    };

    fn rect(x: f64, y: f64, w: f64, h: f64) -> CGRect {
        CGRect::new(CGPoint::new(x, y), CGSize::new(w, h))
    }

    fn wid(idx: u32) -> WindowId {
        WindowId::new(1, idx)
    }

    fn target(idx: u32) -> PictureTarget {
        PictureTarget {
            window: wid(idx),
            server_id: WindowServerId::new(idx),
            size: CGSize::new(859.0, 1081.0),
        }
    }

    fn tag(generation: u64) -> Tag {
        Tag { client: Client(1), generation }
    }

    const LEFT: CGRect = CGRect {
        origin: CGPoint { x: 4.0, y: 32.0 },
        size: CGSize { width: 859.0, height: 1081.0 },
    };
    const RIGHT: CGRect = CGRect {
        origin: CGPoint { x: 865.0, y: 32.0 },
        size: CGSize { width: 859.0, height: 1081.0 },
    };
    /// Parked past the right edge.
    const PARKED: CGRect = CGRect {
        origin: CGPoint { x: 1727.0, y: 32.0 },
        size: CGSize { width: 859.0, height: 1081.0 },
    };

    fn none(_: WindowId) -> Option<Vec<u8>> {
        None
    }

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
        at: impl Fn(WindowId) -> Option<CGRect>,
        band: impl Fn(WindowId) -> Option<u8>,
    ) -> Option<(Vec<Set>, Outcome)> {
        round_of(watches, now, at, |window| band(window).map_or(Capture::Missed, taken))
    }

    /// One round in which what came of each capture is what `capture` says.
    fn round_of(
        watches: &mut Watches,
        now: Instant,
        at: impl Fn(WindowId) -> Option<CGRect>,
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

    fn frame_zero(strip: &[(u32, CGRect)], background: &[(u32, CGRect)]) -> OnScreen {
        OnScreen {
            tag: tag(1),
            moment: Moment::FrameZero,
            strip: strip.iter().map(|(idx, at)| (target(*idx), *at)).collect(),
            background: background.iter().map(|(idx, at)| (target(*idx), *at)).collect(),
            behind: vec![WindowServerId::new(9)],
        }
    }

    fn landing(windows: &[(u32, CGRect)], settle: Settle, until: Instant) -> Watch {
        landing_of(tag(1), vec![WindowServerId::new(9)], windows, settle, until)
    }

    fn landing_of(
        tag: Tag,
        behind: Vec<WindowServerId>,
        windows: &[(u32, CGRect)],
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

    fn change(idx: u32, end: CGRect) -> Watch {
        Watch::Change {
            tag: tag(1),
            window: target(idx),
            end,
            behind: vec![WindowServerId::new(9)],
            every: CHANGE_EVERY,
            tries: CHANGE_TRIES,
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
            &none,
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
            &none,
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
            &none,
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
            &none,
        );
        round(&mut watches, start, |_| Some(LEFT), |_| Some(7)).unwrap();
        watches.stop(WatchId(1));

        watches.ask(frame_zero(&[(1, PARKED)], &[]));
        round(&mut watches, start, |_| Some(PARKED), |_| Some(7)).unwrap();
        watches.watch(
            WatchId(2),
            landing(&[(1, LEFT)], Settle::AsSoonAsThere, start + ms(150)),
            start,
            &none,
        );
        let (_, outcome) = round(&mut watches, start, |_| Some(LEFT), |_| Some(7)).unwrap();
        assert_eq!(delivered(&outcome), vec![(Why::Landed, vec![wid(1)])]);
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
            &none,
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
            &none,
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
            &none,
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
            at += REVEAL_CHASE_INTERVAL;
        };
        assert_eq!(rounds, 4, "1, 2, 3, then 3 again");
        assert_eq!(delivered(&outcome), vec![(Why::Landed, vec![wid(1)])]);
    }

    /// A grow's first capture that already differs from the picture held before it is settled: the
    /// app has repainted.
    #[test]
    fn a_repainting_landing_that_differs_from_before_is_delivered_at_once() {
        let start = Instant::now();
        let mut watches = Watches::default();
        watches.watch(
            WatchId(1),
            landing(&[(1, LEFT)], Settle::Repainted, start + ms(150)),
            start,
            &|_| Some(vec![50]),
        );
        let (_, outcome) = round(&mut watches, start, |_| Some(LEFT), |_| Some(2)).unwrap();
        assert_eq!(delivered(&outcome), vec![(Why::Landed, vec![wid(1)])]);
    }

    /// One that never settles is given up on after the chase's attempts, as the reveal chase did.
    #[test]
    fn a_repainting_landing_that_never_settles_is_lost() {
        let start = Instant::now();
        let mut watches = Watches::default();
        watches.watch(
            WatchId(1),
            landing(&[(1, LEFT)], Settle::Repainted, start + ms(150)),
            start,
            &none,
        );
        let counter = std::cell::Cell::new(0u8);
        let mut at = start;
        let mut lost = Vec::new();
        for _ in 0..REVEAL_CHASE_ATTEMPTS {
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
            at += REVEAL_CHASE_INTERVAL;
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
            &none,
        );
        watches.watch(
            WatchId(2),
            landing(&[(2, RIGHT)], Settle::Repainted, start + ms(150)),
            start,
            &none,
        );
        let at = |w: WindowId| Some(if w == wid(1) { LEFT } else { RIGHT });
        let (_, outcome) = round_of(&mut watches, start, at, |w| {
            if w == wid(1) { Capture::Refused } else { taken(1) }
        })
        .unwrap();
        assert!(outcome.deliveries.is_empty(), "refused, and not settled yet");
        assert_eq!(outcome.lost, vec![(tag(1), wid(1))], "at once, not at the deadline");

        let next = start + REVEAL_CHASE_INTERVAL;
        let (_, outcome) = round_of(&mut watches, next, at, |_| Capture::Refused).unwrap();
        assert!(outcome.deliveries.is_empty());
        assert_eq!(outcome.lost, vec![(tag(1), wid(2))], "settling, refused");
        assert!(watches.next(start + ms(50)).is_none(), "looked at no more");
        assert!(watches.expire(start + ms(150)).is_empty(), "each lost once");
    }

    /// The focus target is captured only once its focus is reported; from then every 15ms, six times
    /// at most, stopping at the first change of its band print.
    #[test]
    fn a_change_watch_captures_after_the_focus_report_until_the_look_changes() {
        let start = Instant::now();
        let mut watches = Watches::default();
        watches.ask(frame_zero(&[(1, LEFT)], &[]));
        round(&mut watches, start, |_| Some(LEFT), |_| Some(1)).unwrap();
        watches.watch(WatchId(1), change(1, LEFT), start, &none);
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
        watches.watch(WatchId(1), change(1, LEFT), start, &none);
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

    /// D1: the focus report often comes before the watch. Remembered for a second, it arms a watch
    /// created after it; older, it does not.
    #[test]
    fn a_focus_reported_just_before_the_watch_arms_it() {
        let start = Instant::now();
        let mut watches = Watches::default();
        watches.hint(Hint::Focused(wid(1)), start);
        watches.watch(WatchId(1), change(1, LEFT), start + ms(900), &none);
        assert!(watches.next(start + ms(900)).is_some(), "900ms after the report");

        let mut watches = Watches::default();
        watches.hint(Hint::Focused(wid(1)), start);
        watches.watch(WatchId(1), change(1, LEFT), start + ms(1100), &none);
        assert!(
            watches.next(start + ms(1100)).is_none(),
            "a report over a second old"
        );

        let mut watches = Watches::default();
        watches.hint(Hint::Focused(wid(2)), start);
        watches.watch(WatchId(1), change(1, LEFT), start, &none);
        assert!(watches.next(start).is_none(), "another window's");
    }

    /// A change capture the cache refuses spends a try and is neither delivered nor judged: the watch
    /// goes on, and is done only once its tries are spent.
    #[test]
    fn a_change_capture_the_cache_refuses_spends_a_try_and_finishes_nothing() {
        let start = Instant::now();
        let mut watches = Watches::default();
        watches.ask(frame_zero(&[(1, LEFT)], &[]));
        round(&mut watches, start, |_| Some(LEFT), |_| Some(1)).unwrap();
        watches.hint(Hint::Focused(wid(1)), start);
        watches.watch(WatchId(1), change(1, LEFT), start, &none);
        let mut at = start;
        for _ in 1..CHANGE_TRIES {
            let (_, outcome) =
                round_of(&mut watches, at, |_| Some(LEFT), |_| Capture::Refused).unwrap();
            assert!(outcome.deliveries.is_empty());
            assert!(watches.next(at + ms(14)).is_none(), "a try spent: the next is 15ms off");
            at += CHANGE_EVERY;
        }
        let (_, outcome) = round(&mut watches, at, |_| Some(LEFT), |_| Some(2)).unwrap();
        assert_eq!(
            delivered(&outcome),
            vec![(Why::FocusChanged, vec![wid(1)])],
            "the last try"
        );

        let mut watches = Watches::default();
        watches.hint(Hint::Focused(wid(1)), start);
        watches.watch(WatchId(1), change(1, LEFT), start, &none);
        let mut at = start;
        let mut tries = 0;
        while let Some((_, outcome)) =
            round_of(&mut watches, at, |_| Some(LEFT), |_| Capture::Refused)
        {
            assert!(outcome.deliveries.is_empty(), "not delivered as landed either");
            tries += 1;
            at += CHANGE_EVERY;
        }
        assert_eq!(tries, CHANGE_TRIES);
    }

    /// Until the window is where it ends, a change watch only watches it land: no try is spent.
    #[test]
    fn a_change_watch_spends_no_try_before_its_window_is_there() {
        let start = Instant::now();
        let mut watches = Watches::default();
        watches.hint(Hint::Focused(wid(1)), start);
        watches.watch(WatchId(1), change(1, LEFT), start, &none);
        let mut at = start;
        for _ in 0..20 {
            round(
                &mut watches,
                at,
                |_| Some(PARKED.intersection(&LAPTOP).union(&rect(900.0, 32.0, 1.0, 1.0))),
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
        watches.watch(WatchId(1), change(1, end), start, &none);
        let (_, outcome) = round(&mut watches, start, |_| Some(end), |_| Some(1)).unwrap();
        assert!(outcome.deliveries.is_empty());
        assert_eq!(outcome.clipped, vec![(tag(1), target(1), end)]);
        assert!(watches.next(start + ms(100)).is_none(), "once");
    }

    /// Windows behind the strip refresh every 16ms, every capture delivered: content changes below the
    /// band, which a band print would never see.
    #[test]
    fn the_background_refreshes_every_sixteen_milliseconds_and_delivers_each_capture() {
        let start = Instant::now();
        let mut watches = Watches::default();
        watches.ask(frame_zero(&[], &[(4, LEFT)]));
        round(&mut watches, start, |_| Some(LEFT), |_| Some(1)).unwrap();
        watches.watch(WatchId(1), live(&[4], None, None), start, &none);
        let (sets, outcome) = round(&mut watches, start, |_| Some(LEFT), |_| Some(1)).unwrap();
        assert!(sets[0].behind.is_empty());
        assert_eq!(
            delivered(&outcome),
            vec![(Why::Background, vec![wid(4)])],
            "unchanged, delivered"
        );
        assert!(watches.next(start + ms(15)).is_none());
        assert!(round(&mut watches, start + ms(16), |_| Some(LEFT), |_| Some(1)).is_some());
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
            &none,
        );
        watches.watch(WatchId(2), live(&[4], None, None), start, &none);
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
        watches.watch(WatchId(1), change(1, LEFT), start, &none);
        watches.watch(WatchId(2), live(&[4], None, None), start, &none);
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
        watches.watch(WatchId(1), change(1, LEFT), start, &none);
        watches.watch(
            WatchId(2),
            live(&[4], Some(WatchId(1)), Some(start + ms(30))),
            start,
            &none,
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

    /// A stopped change watch holds the background up no longer.
    #[test]
    fn the_background_follows_a_change_watch_that_was_stopped() {
        let start = Instant::now();
        let mut watches = Watches::default();
        watches.watch(WatchId(1), change(1, LEFT), start, &none);
        watches.watch(WatchId(2), live(&[4], Some(WatchId(1)), None), start, &none);
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
        watches.watch(WatchId(1), change(1, LEFT), start, &none);
        watches.watch(
            WatchId(2),
            landing(&[(1, LEFT), (2, RIGHT)], Settle::AsSoonAsThere, start + ms(150)),
            start,
            &none,
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
            &none,
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
            &none,
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
            &none,
        );
        let sets = watches.next(start + ms(50)).unwrap();
        assert_eq!(looked(&sets), vec![vec![wid(2), wid(3)]]);

        let mut watches = Watches::default();
        watches.hint(Hint::Focused(wid(1)), start);
        watches.watch(WatchId(1), change(1, LEFT), start, &none);
        watches.retarget(WatchId(1), change(1, RIGHT), start + ms(2000), &none);
        assert!(
            watches.next(start + ms(2000)).is_some(),
            "armed still, long after the report"
        );
        watches.retarget(WatchId(1), change(2, RIGHT), start + ms(2000), &none);
        assert!(
            watches.next(start + ms(2000)).is_none(),
            "another window: not armed"
        );
    }

    /// A repainting window still landing settles, through a retarget, against the picture held when
    /// the watch first named it: by then the cache holds the watch's own captures. A window new to the
    /// watch settles against the picture held now.
    #[test]
    fn a_retarget_keeps_the_picture_a_repainting_window_settles_against() {
        let start = Instant::now();
        let mut watches = Watches::default();
        watches.watch(
            WatchId(1),
            landing(&[(1, LEFT)], Settle::Repainted, start + ms(150)),
            start,
            &|_| Some(vec![50]),
        );
        watches.retarget(
            WatchId(1),
            landing(&[(1, LEFT), (2, RIGHT)], Settle::Repainted, start + ms(150)),
            start,
            &|_| Some(vec![100]),
        );
        let (_, outcome) = round(
            &mut watches,
            start,
            |w| Some(if w == wid(1) { LEFT } else { RIGHT }),
            |_| Some(2),
        )
        .unwrap();
        assert_eq!(
            delivered(&outcome),
            vec![(Why::Landed, vec![wid(1)])],
            "window 1 differs from the picture before; window 2 is what is held now"
        );
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
            &none,
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
            &none,
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
            &none,
        );
        watches.ask(frame_zero(&[(1, LEFT)], &[]));
        watches.forget(wid(1));
        let (sets, outcome) = round(&mut watches, start, |_| Some(LEFT), |_| Some(1)).unwrap();
        assert_eq!(looked(&sets), vec![vec![]], "frame zero without it");
        assert_eq!(delivered(&outcome), vec![(Why::FrameZero, vec![])]);
        assert_eq!(looked(&watches.next(start).unwrap()), vec![vec![wid(2)]]);
        watches.stop(WatchId(1));
        watches.watch(WatchId(2), change(1, LEFT), start, &none);
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
}
