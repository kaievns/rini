//! The capture schedule: which call the pictures service makes next.
//!
//! Pure. Asks, watches, hints, holds, display changes and what each call brought back go in, with `now`
//! wherever time matters; the next call comes out. The SkyLight half, windows on screen captured as a
//! transition asks and watches, is `crate::pictures::domain::watch`; this orders it ahead of the
//! ScreenCaptureKit half. `crate::pictures::platform::service` makes the calls, on its own thread. The
//! rules and why each holds: "The schedule" and "The watches" in `src/pictures/docs/README.md`.

use std::time::{Duration, Instant};

use objc2_core_foundation::{CGRect, CGSize};
use rini_core::ids::WindowId;

use crate::pictures::domain::coverage::{Coverage, needs_capture, picture_is_stale};
use crate::pictures::domain::target::PictureTarget;
use crate::pictures::domain::watch::{Hint, OnScreen, Outcome, Seen, Set, Tag, Watch, WatchId, Watches};

/// How urgently windows are wanted, most urgent first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Priority {
    /// A transition composing now has no picture of these windows.
    FrameZero,
    /// The two ends of a focus change, whose rendering changed and whose size did not.
    Focus,
    /// Every window on the display, after a display change or on the debug command.
    Background,
    /// Windows named for a later transition or the switcher: skipped where the picture held fits and is
    /// fresh.
    Warm,
}

impl Priority {
    /// Frame-zero and focus captures start through a hold, and at once: the transition holding the
    /// service is the one waiting for them.
    fn wanted_now(self) -> bool { matches!(self, Self::FrameZero | Self::Focus) }
}

/// Windows wanted, at one priority.
#[derive(Debug, Clone)]
pub struct Ask {
    pub priority: Priority,
    pub targets: Vec<PictureTarget>,
}

/// What a transition is doing to the service.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hold {
    /// The overlay is up: only frame-zero and focus work starts, and the bar is not pictured, since a
    /// picture of it is read off the framebuffer the overlay covers.
    Transition,
    /// The overlay has lifted and the transition is settling: the bar is pictured again, and the rest
    /// still waits.
    Settling,
    /// Nothing holds the service, and what waited starts, most urgent first.
    Released,
}

/// The display the overlay flies on, as far as a picture of it is concerned.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Display {
    pub id: u32,
    /// The whole display, in global coordinates.
    pub frame: CGRect,
    pub scale: f64,
}

/// What the cache holds of a window that is worth drawing, as the warm filter reads it.
#[derive(Debug, Clone, Copy)]
pub struct Held {
    pub coverage: Coverage,
    pub taken: Instant,
}

/// Whether a warm captures a window drawn at `size`: nothing worth drawing is held, what is held no
/// longer fits, or it is stale. See "A window that was resized keeps a usable picture" in
/// `src/pictures/docs/capture-research.md`.
pub fn wants_warm(held: Option<Held>, size: CGSize, now: Instant) -> bool {
    needs_capture(held.map(|held| held.coverage), (size.width, size.height))
        || held.is_some_and(|held| picture_is_stale(now.saturating_duration_since(held.taken)))
}

/// How long a background or warm ask waits before it is listed. A pass's warm is sent just after the
/// flight it starts, and so reaches the service before that flight's hold does; listed at once, it
/// would put ScreenCaptureKit to work at the flight's frame zero. A flight that composites its desktop
/// on the main thread can hold later than this; what is listed meanwhile is listed again at the
/// release (`hold`).
pub const SETTLE_BEFORE_LISTING: Duration = Duration::from_millis(50);

/// Concurrent ScreenCaptureKit captures. It serialises internally, so wall clock stops improving past
/// four. See "Capture cost" in `src/pictures/docs/capture-research.md`.
pub const MAX_CONCURRENT: usize = 4;

/// How long after the overlay lifts, or the display changes, the bar is pictured: a bar composite is
/// too slow (31ms median) to pay per switch, so a burst of switches pays it once, and the compositor
/// has dropped the hidden overlay from the framebuffer by then.
pub const BAR_REFRESH_DELAY: Duration = Duration::from_millis(250);

/// Chase poll interval and attempt budget for a growing window's real frame (about a second).
/// See "A grow holds, then reveals" in `src/transitions/docs/coordination.md`.
pub const REVEAL_CHASE_INTERVAL: Duration = Duration::from_millis(8);

pub const REVEAL_CHASE_ATTEMPTS: usize = 125;

/// How many new windows one pass captures synchronously at spawn; the rest take a reservation.
/// See "A window that opens" in `src/transitions/docs/coordination.md`.
pub const MAX_SYNC_ENTRANCE_CAPTURES: usize = 4;

/// The service's next call. `P` is what the platform prepared for capturing a window its listing found.
#[derive(Debug)]
pub enum Next<P> {
    /// Tell these clients their windows did not land by the deadline they gave.
    Lost(Vec<(Tag, WindowId)>),
    /// Read these windows' server frames, capture those where they are wanted through SkyLight, set by
    /// set, and say what was seen (`Schedule::seen`).
    Sky(Vec<Set>),
    /// Find these windows in one ScreenCaptureKit listing, and prepare a capture of each.
    List(Vec<PictureTarget>),
    /// Capture a window the listing found.
    Capture { target: PictureTarget, prepared: P },
    /// Render the desktop of this display.
    Desktop(Display),
    /// Picture the bar of this display.
    Bar(Display),
    /// Nothing before then, unless something arrives.
    WaitUntil(Instant),
    /// Nothing until something arrives.
    Idle,
}

/// Every capture owed and under way, and the holds over them.
pub struct Schedule<P> {
    hold: Hold,
    watches: Watches,
    jobs: Vec<Job<P>>,
    asked: u64,
    display: Option<Display>,
    picture_bar: bool,
    desktop: Desktop,
    bar_due: Option<Instant>,
}

/// One window owed a capture.
struct Job<P> {
    target: PictureTarget,
    priority: Priority,
    /// When it was first asked for, so equal priorities start in the order they were asked.
    order: u64,
    asked_at: Instant,
    stage: Stage<P>,
    /// Forgotten while its listing or capture was out, so what comes back is not kept.
    forgotten: bool,
    /// Clients owed the picture as a focus change, with the server frame they asked at.
    deliver: Vec<(Tag, CGRect)>,
}

impl<P> Job<P> {
    /// When a waiting job may be listed: at once for what a transition waits on, once settled for
    /// the rest.
    fn listable_from(&self) -> Instant {
        if self.priority.wanted_now() {
            self.asked_at
        } else {
            self.asked_at + SETTLE_BEFORE_LISTING
        }
    }
}

enum Stage<P> {
    Waiting,
    Listing,
    Listed(P),
    Capturing,
}

#[derive(Default)]
struct Desktop {
    wanted: bool,
    /// A render is out.
    rendering: bool,
    /// When the render held for the current display was taken.
    taken: Option<Instant>,
}

impl<P> Default for Schedule<P> {
    fn default() -> Self {
        Self {
            hold: Hold::Released,
            watches: Watches::default(),
            jobs: Vec::new(),
            asked: 0,
            display: None,
            picture_bar: false,
            desktop: Desktop::default(),
            bar_due: None,
        }
    }
}

impl<P> Schedule<P> {
    /// Owes each target a capture at `ask.priority`. A window already owed one is not asked twice: it
    /// keeps the more urgent priority, and the latest size while it still waits. One listed at another
    /// size is listed again: what the listing prepared is a buffer of the size it found.
    pub fn ask(&mut self, ask: Ask, now: Instant) {
        for target in ask.targets {
            self.asked += 1;
            match self.jobs.iter_mut().find(|job| job.target.window == target.window) {
                Some(job) => {
                    job.priority = job.priority.min(ask.priority);
                    job.forgotten = false;
                    let resized = job.target.size != target.size;
                    match job.stage {
                        Stage::Waiting => job.target = target,
                        Stage::Listed(_) if resized => {
                            job.target = target;
                            job.stage = Stage::Waiting;
                        }
                        _ => {}
                    }
                }
                None => self.jobs.push(Job {
                    target,
                    priority: ask.priority,
                    order: self.asked,
                    asked_at: now,
                    stage: Stage::Waiting,
                    forgotten: false,
                    deliver: Vec::new(),
                }),
            }
        }
    }

    /// A capture listed before a hold and unable to start through it is listed again at the release:
    /// the transition may resize its window, and the buffer the listing prepared would not fit it.
    pub fn hold(&mut self, hold: Hold, now: Instant) {
        self.hold = hold;
        if hold != Hold::Released {
            for job in &mut self.jobs {
                if matches!(job.stage, Stage::Listed(_)) && !job.priority.wanted_now() {
                    job.stage = Stage::Waiting;
                }
            }
        }
        match hold {
            Hold::Transition => self.bar_due = None,
            Hold::Settling => self.arm_bar(now),
            Hold::Released => {
                let stale = self
                    .desktop
                    .taken
                    .is_none_or(|taken| picture_is_stale(now.saturating_duration_since(taken)));
                if stale {
                    self.desktop.wanted = true;
                }
            }
        }
    }

    /// Windows on screen, wanted now: see [`OnScreen`].
    pub fn ask_on_screen(&mut self, ask: OnScreen) { self.watches.ask(ask) }

    /// Starts `watch`. `before` is the thumbprint of the picture held of a window, which a repainting
    /// landing settles against.
    pub fn watch(&mut self, id: WatchId, watch: Watch, now: Instant, before: &dyn Fn(WindowId) -> Option<Vec<u8>>) {
        self.watches.watch(id, watch, now, before)
    }

    pub fn retarget(&mut self, id: WatchId, watch: Watch, now: Instant, before: &dyn Fn(WindowId) -> Option<Vec<u8>>) {
        self.watches.retarget(id, watch, now, before)
    }

    pub fn stop(&mut self, id: WatchId) { self.watches.stop(id) }

    pub fn hint(&mut self, hint: Hint, now: Instant) { self.watches.hint(hint, now) }

    /// What the window server and the captures said of the windows `Next::Sky` named. A window a
    /// changing watch found cut by the display's edge is owed one ScreenCaptureKit capture, which starts
    /// through any hold.
    pub fn seen(&mut self, now: Instant, seen: &[Seen]) -> Outcome {
        let mut outcome = self.watches.seen(now, seen, self.display.map(|display| display.frame));
        for (tag, target, frame) in std::mem::take(&mut outcome.clipped) {
            self.ask_clipped(tag, target, frame, now);
        }
        outcome
    }

    /// One capture of `target` for `tag`'s focus change. A capture already out began before the focus,
    /// so it is followed by one of its own.
    fn ask_clipped(&mut self, tag: Tag, target: PictureTarget, frame: CGRect, now: Instant) {
        self.asked += 1;
        let waiting = self.jobs.iter_mut().find(|job| {
            job.target.window == target.window && matches!(job.stage, Stage::Waiting | Stage::Listed(_))
        });
        match waiting {
            Some(job) => {
                job.priority = job.priority.min(Priority::Focus);
                job.forgotten = false;
                job.deliver.push((tag, frame));
            }
            None => self.jobs.push(Job {
                target,
                priority: Priority::Focus,
                order: self.asked,
                asked_at: now,
                stage: Stage::Waiting,
                forgotten: false,
                deliver: vec![(tag, frame)],
            }),
        }
    }

    /// A window that is gone: nothing more is captured of it, and nothing already out is kept.
    pub fn forget(&mut self, window: WindowId) {
        self.watches.forget(window);
        self.jobs.retain_mut(|job| {
            if job.target.window != window {
                return true;
            }
            match job.stage {
                Stage::Waiting | Stage::Listed(_) => false,
                Stage::Listing | Stage::Capturing => {
                    job.forgotten = true;
                    true
                }
            }
        });
    }

    /// The display the overlay now flies on. True when it changed, which leaves the pictures of the
    /// display left behind with nothing to do: its desktop is rendered afresh and its bar pictured
    /// `BAR_REFRESH_DELAY` later. Window captures belong to no display and carry on.
    pub fn set_display(&mut self, display: Display, picture_bar: bool, now: Instant) -> bool {
        let changed = self.display != Some(display);
        self.picture_bar = picture_bar;
        if changed {
            self.display = Some(display);
            self.desktop.wanted = true;
            self.desktop.taken = None;
            self.arm_bar(now);
        }
        if !picture_bar {
            self.bar_due = None;
        }
        changed
    }

    /// What the listing found of `window`: what it prepared for the capture, or `None` when the window
    /// was not listed. A listing that returns into a hold its capture cannot start through is listed
    /// again at the release, like one listed before the hold.
    pub fn listed(&mut self, window: WindowId, prepared: Option<P>) {
        let Some(at) = self.position(window, |stage| matches!(stage, Stage::Listing)) else {
            return;
        };
        let held = self.hold != Hold::Released && !self.jobs[at].priority.wanted_now();
        match prepared {
            Some(_) if held && !self.jobs[at].forgotten => self.jobs[at].stage = Stage::Waiting,
            Some(prepared) if !self.jobs[at].forgotten => {
                self.jobs[at].stage = Stage::Listed(prepared);
            }
            _ => {
                self.jobs.remove(at);
            }
        }
    }

    /// A capture of `window` came back. `None` when what it brought may not be kept; else the clients
    /// owed it as a focus change, with the server frame each asked at.
    pub fn captured(&mut self, window: WindowId) -> Option<Vec<(Tag, CGRect)>> {
        let at = self.position(window, |stage| matches!(stage, Stage::Capturing))?;
        let job = self.jobs.remove(at);
        (!job.forgotten).then_some(job.deliver)
    }

    /// A render of `display`'s desktop came back, taken at `taken`, or not at all. Whether it may be
    /// kept: only a render of the display the overlay is on now.
    pub fn desktop_rendered(&mut self, display: Display, taken: Option<Instant>) -> bool {
        self.desktop.rendering = false;
        let kept = self.display == Some(display) && taken.is_some();
        if kept {
            self.desktop.taken = taken;
        }
        kept
    }

    /// The display the overlay flies on, whose desktop and bar the service holds.
    pub fn display(&self) -> Option<Display> { self.display }

    /// Whether a transition holds the service.
    pub fn held(&self) -> bool { self.hold != Hold::Released }

    /// Every window owed a capture or with one out, but for those forgotten.
    pub fn owed(&self) -> impl Iterator<Item = WindowId> + '_ {
        self.jobs.iter().filter(|job| !job.forgotten).map(|job| job.target.window)
    }

    /// The next call to make. `held` is what the cache holds worth drawing of a window, for the warm
    /// filter.
    pub fn next(&mut self, now: Instant, held: impl Fn(WindowId) -> Option<Held>) -> Next<P> {
        // What a transition waits on starts before any ScreenCaptureKit call is issued: queued
        // ScreenCaptureKit work slows every SkyLight call behind it.
        let lost = self.watches.expire(now);
        if !lost.is_empty() {
            return Next::Lost(lost);
        }
        if let Some(sets) = self.watches.next(now) {
            return Next::Sky(sets);
        }
        let hold = self.hold;
        let may_start = |priority: Priority| hold == Hold::Released || priority.wanted_now();
        let listable = |job: &Job<P>| {
            matches!(job.stage, Stage::Waiting)
                && may_start(job.priority)
                && job.listable_from() <= now
        };

        self.jobs.retain(|job| {
            !(listable(job)
                && job.priority == Priority::Warm
                && !wants_warm(held(job.target.window), job.target.size, now))
        });
        let mut listing: Vec<&mut Job<P>> =
            self.jobs.iter_mut().filter(|job| listable(job)).collect();
        if !listing.is_empty() {
            listing.sort_by_key(|job| (job.priority, job.order));
            let targets = listing
                .into_iter()
                .map(|job| {
                    job.stage = Stage::Listing;
                    job.target
                })
                .collect();
            return Next::List(targets);
        }

        let capturing = self.jobs.iter().filter(|job| matches!(job.stage, Stage::Capturing)).count();
        if capturing < MAX_CONCURRENT
            && let Some(job) = self
                .jobs
                .iter_mut()
                .filter(|job| matches!(job.stage, Stage::Listed(_)) && may_start(job.priority))
                .min_by_key(|job| (job.priority, job.order))
            && let Stage::Listed(prepared) = std::mem::replace(&mut job.stage, Stage::Capturing)
        {
            return Next::Capture { target: job.target, prepared };
        }

        if hold == Hold::Released
            && self.desktop.wanted
            && !self.desktop.rendering
            && let Some(display) = self.display
        {
            self.desktop.wanted = false;
            self.desktop.rendering = true;
            return Next::Desktop(display);
        }

        if let (Some(due), Some(display)) = (self.bar_due, self.display)
            && due <= now
        {
            self.bar_due = None;
            return Next::Bar(display);
        }
        let settling = self
            .jobs
            .iter()
            .filter(|job| matches!(job.stage, Stage::Waiting) && may_start(job.priority))
            .map(Job::listable_from);
        let watching = self.watches.wake(now);
        match self.bar_due.filter(|_| self.display.is_some()).into_iter().chain(settling).chain(watching).min() {
            Some(at) => Next::WaitUntil(at),
            None => Next::Idle,
        }
    }

    /// The bar is due `BAR_REFRESH_DELAY` from `now`, unless it is not pictured or the overlay is up.
    fn arm_bar(&mut self, now: Instant) {
        if self.picture_bar && self.display.is_some() && self.hold != Hold::Transition {
            self.bar_due = Some(now + BAR_REFRESH_DELAY);
        }
    }

    fn position(&self, window: WindowId, at: impl Fn(&Stage<P>) -> bool) -> Option<usize> {
        self.jobs.iter().position(|job| job.target.window == window && at(&job.stage))
    }
}

#[cfg(test)]
mod tests {
    use objc2_core_foundation::CGPoint;
    use rini_core::ids::WindowServerId;

    use super::*;

    fn wid(idx: u32) -> WindowId { WindowId::new(1, idx) }

    fn target(idx: u32, width: f64) -> PictureTarget {
        PictureTarget {
            window: wid(idx),
            server_id: WindowServerId::new(idx),
            size: CGSize::new(width, 1081.0),
        }
    }

    fn ask(priority: Priority, windows: &[u32]) -> Ask {
        Ask {
            priority,
            targets: windows.iter().map(|idx| target(*idx, 859.0)).collect(),
        }
    }

    fn built_in() -> Display {
        Display {
            id: 1,
            frame: CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(1728.0, 1117.0)),
            scale: 2.0,
        }
    }

    fn external() -> Display {
        Display {
            id: 2,
            frame: CGRect::new(CGPoint::new(-670.0, -1692.0), CGSize::new(3008.0, 1692.0)),
            scale: 2.0,
        }
    }

    fn nothing_held(_: WindowId) -> Option<Held> { None }

    /// Long enough before `now` for a background or warm ask to have settled.
    fn settled_by(now: Instant) -> Instant { now - SETTLE_BEFORE_LISTING }

    fn listed_windows(next: Next<()>) -> Vec<WindowId> {
        match next {
            Next::List(targets) => targets.into_iter().map(|t| t.window).collect(),
            other => panic!("expected a listing, got {other:?}"),
        }
    }

    /// Lists and prepares everything that may start, then hands out the captures one by one.
    fn captures(schedule: &mut Schedule<()>, now: Instant) -> Vec<WindowId> {
        let mut started = Vec::new();
        loop {
            match schedule.next(now, nothing_held) {
                Next::List(targets) => {
                    for target in targets {
                        schedule.listed(target.window, Some(()));
                    }
                }
                Next::Capture { target, .. } => started.push(target.window),
                _ => return started,
            }
        }
    }

    /// From frame zero to the release nothing starts but what the transition waits for: its frame
    /// zero and the two ends of its focus change. The engine's own chases and refreshes never come
    /// here.
    #[test]
    fn held_work_waits_and_only_frame_zero_and_focus_start_through_a_hold() {
        for hold in [Hold::Transition, Hold::Settling] {
            let now = Instant::now();
            let mut schedule = Schedule::<()>::default();
            schedule.hold(hold, now);
            schedule.ask(ask(Priority::Warm, &[1]), settled_by(now));
            schedule.ask(ask(Priority::Background, &[2]), settled_by(now));
            schedule.ask(ask(Priority::Focus, &[3]), settled_by(now));
            schedule.ask(ask(Priority::FrameZero, &[4]), settled_by(now));
            assert_eq!(captures(&mut schedule, now), vec![wid(4), wid(3)], "{hold:?}");

            schedule.hold(Hold::Released, now);
            assert_eq!(captures(&mut schedule, now), vec![wid(2), wid(1)], "released, in order");
        }
    }

    /// The full table, hold by priority: what starts.
    #[test]
    fn what_starts_under_each_hold() {
        const PRIORITIES: [Priority; 4] = [
            Priority::FrameZero,
            Priority::Focus,
            Priority::Background,
            Priority::Warm,
        ];
        let mut started = 0usize;
        for hold in [Hold::Released, Hold::Transition, Hold::Settling] {
            for priority in PRIORITIES {
                let now = Instant::now();
                let mut schedule = Schedule::<()>::default();
                schedule.hold(hold, now);
                schedule.ask(ask(priority, &[1]), settled_by(now));
                let starts = !captures(&mut schedule, now).is_empty();
                let expected = hold == Hold::Released
                    || matches!(priority, Priority::FrameZero | Priority::Focus);
                assert_eq!(starts, expected, "{hold:?}/{priority:?}");
                started += starts as usize;
            }
        }
        // Released 4, each hold 2.
        assert_eq!(started, 8);
    }

    /// A pass's warm reaches the service before the hold of the flight that pass started, so it waits
    /// long enough for that hold to arrive, and then out the flight.
    #[test]
    fn a_warm_asked_just_before_its_flight_holds_waits_out_the_flight() {
        let now = Instant::now();
        let mut schedule = Schedule::<()>::default();
        schedule.ask(ask(Priority::Warm, &[1]), now);
        assert!(matches!(
            schedule.next(now, nothing_held),
            Next::WaitUntil(at) if at == now + SETTLE_BEFORE_LISTING
        ));
        schedule.hold(Hold::Transition, now + Duration::from_millis(10));
        assert!(
            matches!(schedule.next(now + SETTLE_BEFORE_LISTING, nothing_held), Next::Idle),
            "held"
        );
        let release = now + Duration::from_millis(500);
        schedule.hold(Hold::Released, release);
        assert_eq!(listed_windows(schedule.next(release, nothing_held)), vec![wid(1)]);
    }

    #[test]
    fn frame_zero_and_focus_are_listed_at_once() {
        for priority in [Priority::FrameZero, Priority::Focus] {
            let now = Instant::now();
            let mut schedule = Schedule::<()>::default();
            schedule.ask(ask(priority, &[1]), now);
            assert_eq!(listed_windows(schedule.next(now, nothing_held)), vec![wid(1)]);
        }
    }

    /// A capture listed before the transition does not start inside it either: ScreenCaptureKit work
    /// queued at a focus change slows every step of it. It is listed again at the release.
    #[test]
    fn a_capture_listed_before_the_transition_waits_for_its_release() {
        let now = Instant::now();
        let mut schedule = Schedule::<()>::default();
        schedule.ask(ask(Priority::Warm, &[1, 2, 3, 4, 5, 6]), settled_by(now));
        let Next::List(targets) = schedule.next(now, nothing_held) else { panic!("a listing") };
        for target in targets {
            schedule.listed(target.window, Some(()));
        }
        schedule.hold(Hold::Transition, now);
        assert!(matches!(schedule.next(now, nothing_held), Next::Idle));
        schedule.hold(Hold::Released, now);
        assert_eq!(captures(&mut schedule, now).len(), MAX_CONCURRENT);
    }

    /// The reported case: a warm listed just before a flight sat through it with a buffer sized at
    /// the listing, so a window the flight resized was captured into the old size at the release, and
    /// the warm the flight asked for it was folded into that stale job. Held, it is listed again.
    #[test]
    fn a_capture_listed_before_a_hold_is_listed_again_at_the_release() {
        let now = Instant::now();
        let mut schedule = Schedule::<()>::default();
        schedule.ask(ask(Priority::Warm, &[1]), settled_by(now));
        assert_eq!(listed_windows(schedule.next(now, nothing_held)), vec![wid(1)]);
        schedule.listed(wid(1), Some(()));
        schedule.hold(Hold::Transition, now);
        schedule.ask(
            Ask {
                priority: Priority::Warm,
                targets: vec![target(1, 1147.0)],
            },
            now,
        );
        schedule.hold(Hold::Released, now);
        let Next::List(targets) = schedule.next(now, nothing_held) else { panic!("a listing") };
        let asked: Vec<(WindowId, f64)> = targets.iter().map(|t| (t.window, t.size.width)).collect();
        assert_eq!(asked, vec![(wid(1), 1147.0)], "listed again, at the size asked last");
    }

    /// A listing that returns once the hold is up has prepared a buffer the flight may outgrow too.
    #[test]
    fn a_listing_that_returns_into_a_hold_is_listed_again_at_the_release() {
        let now = Instant::now();
        let mut schedule = Schedule::<()>::default();
        schedule.ask(ask(Priority::Warm, &[1]), settled_by(now));
        let _ = schedule.next(now, nothing_held);
        schedule.hold(Hold::Transition, now);
        schedule.listed(wid(1), Some(()));
        assert!(matches!(schedule.next(now, nothing_held), Next::Idle), "held");
        schedule.hold(Hold::Released, now);
        assert_eq!(listed_windows(schedule.next(now, nothing_held)), vec![wid(1)]);
    }

    /// Asked again at another size, a listed window is listed again; at the same size its listing
    /// still serves.
    #[test]
    fn a_listed_window_asked_for_at_another_size_is_listed_again() {
        let now = Instant::now();
        let mut schedule = Schedule::<()>::default();
        schedule.ask(ask(Priority::Warm, &[1, 2]), settled_by(now));
        let _ = schedule.next(now, nothing_held);
        schedule.listed(wid(1), Some(()));
        schedule.listed(wid(2), Some(()));
        schedule.ask(
            Ask {
                priority: Priority::Warm,
                targets: vec![target(1, 1147.0), target(2, 859.0)],
            },
            settled_by(now),
        );
        let Next::List(targets) = schedule.next(now, nothing_held) else { panic!("a listing") };
        assert_eq!(
            targets.iter().map(|t| (t.window, t.size.width)).collect::<Vec<_>>(),
            vec![(wid(1), 1147.0)]
        );
        assert!(matches!(schedule.next(now, nothing_held), Next::Capture { target, .. } if target.window == wid(2)));
    }

    /// A warm owed when the focus moves is not asked twice: it takes the focus priority, and starts
    /// through the hold the warm alone would wait out. Whether it waits or is listed already.
    #[test]
    fn a_held_warm_asked_again_at_focus_starts_through_the_hold() {
        let now = Instant::now();
        let mut waiting = Schedule::<()>::default();
        waiting.ask(ask(Priority::Warm, &[1]), settled_by(now));
        waiting.hold(Hold::Transition, now);
        waiting.ask(ask(Priority::Focus, &[1]), now);
        assert_eq!(captures(&mut waiting, now), vec![wid(1)], "waiting");

        let mut listed = Schedule::<()>::default();
        listed.ask(ask(Priority::Warm, &[1]), settled_by(now));
        let _ = listed.next(now, nothing_held);
        listed.listed(wid(1), Some(()));
        listed.hold(Hold::Transition, now);
        listed.ask(ask(Priority::Focus, &[1]), now);
        assert_eq!(captures(&mut listed, now), vec![wid(1)], "listed");
    }

    /// Forgotten while its capture was out and asked for again, a window's capture is kept after all.
    #[test]
    fn a_forgotten_window_asked_for_again_keeps_its_capture() {
        let now = Instant::now();
        let mut schedule = Schedule::<()>::default();
        schedule.ask(ask(Priority::Warm, &[1]), settled_by(now));
        assert_eq!(captures(&mut schedule, now), vec![wid(1)]);
        schedule.forget(wid(1));
        schedule.ask(ask(Priority::Warm, &[1]), settled_by(now));
        assert!(schedule.owed().any(|window| window == wid(1)));
        assert!(schedule.captured(wid(1)).is_some());
    }

    /// A window asked for twice while it waits is captured once, at the size asked for last.
    #[test]
    fn a_window_asked_for_twice_while_it_waits_is_captured_once_at_the_latest_size() {
        let now = Instant::now();
        let mut schedule = Schedule::<()>::default();
        schedule.hold(Hold::Transition, now);
        schedule.ask(Ask {
            priority: Priority::Warm,
            targets: vec![target(1, 859.0), target(2, 859.0)],
        }, settled_by(now));
        schedule.ask(Ask {
            priority: Priority::Warm,
            targets: vec![target(1, 1147.0), target(3, 859.0)],
        }, settled_by(now));
        schedule.hold(Hold::Released, now);
        let Next::List(targets) = schedule.next(now, nothing_held) else { panic!("a listing") };
        let asked: Vec<(WindowId, f64)> = targets.iter().map(|t| (t.window, t.size.width)).collect();
        assert_eq!(asked, vec![(wid(1), 1147.0), (wid(2), 859.0), (wid(3), 859.0)]);
        assert!(
            matches!(schedule.next(now, nothing_held), Next::Idle),
            "drained once"
        );
    }

    /// A warm asked for a window whose picture fits and is fresh is not worth a capture; one that no
    /// longer fits, or has gone stale, is.
    #[test]
    fn a_warm_captures_only_what_needs_capturing_or_has_gone_stale() {
        let now = Instant::now();
        let fits = Held {
            coverage: Coverage {
                covered: (859.0, 1081.0),
                window: (859.0, 1081.0),
            },
            taken: now,
        };
        assert!(!wants_warm(Some(fits), CGSize::new(859.0, 1081.0), now));
        assert!(wants_warm(Some(fits), CGSize::new(1147.0, 1081.0), now), "resized");
        assert!(wants_warm(None, CGSize::new(859.0, 1081.0), now), "nothing held");
        let later = now + Duration::from_secs(3);
        assert!(wants_warm(Some(fits), CGSize::new(859.0, 1081.0), later), "stale");

        let mut schedule = Schedule::<()>::default();
        schedule.ask(ask(Priority::Warm, &[1, 2]), settled_by(now));
        let held = |window: WindowId| (window == wid(1)).then_some(fits);
        assert_eq!(listed_windows(schedule.next(now, held)), vec![wid(2)]);
        assert!(schedule.owed().all(|window| window != wid(1)), "dropped, not kept waiting");
    }

    /// The focus refresh, the frame-zero list and the display's visible windows are captured whatever
    /// is held: none of them is about a size the cache could already have.
    #[test]
    fn only_a_warm_is_filtered() {
        let now = Instant::now();
        let fits = Held {
            coverage: Coverage {
                covered: (859.0, 1081.0),
                window: (859.0, 1081.0),
            },
            taken: now,
        };
        for priority in [Priority::FrameZero, Priority::Focus, Priority::Background] {
            let mut schedule = Schedule::<()>::default();
            schedule.ask(ask(priority, &[1]), settled_by(now));
            assert_eq!(listed_windows(schedule.next(now, |_| Some(fits))), vec![wid(1)]);
        }
    }

    #[test]
    fn a_window_being_captured_is_not_captured_again() {
        let now = Instant::now();
        let mut schedule = Schedule::<()>::default();
        schedule.ask(ask(Priority::Warm, &[1]), settled_by(now));
        assert_eq!(captures(&mut schedule, now), vec![wid(1)]);
        schedule.ask(ask(Priority::Focus, &[1]), settled_by(now));
        assert!(captures(&mut schedule, now).is_empty(), "the one out serves both");
        assert!(schedule.captured(wid(1)).is_some());
        schedule.ask(ask(Priority::Focus, &[1]), settled_by(now));
        assert_eq!(captures(&mut schedule, now), vec![wid(1)], "asked again once it is back");
    }

    #[test]
    fn at_most_four_captures_are_out_at_once() {
        let now = Instant::now();
        let mut schedule = Schedule::<()>::default();
        schedule.ask(ask(Priority::Warm, &[1, 2, 3, 4, 5, 6]), settled_by(now));
        assert_eq!(captures(&mut schedule, now), vec![wid(1), wid(2), wid(3), wid(4)]);
        assert!(schedule.captured(wid(2)).is_some());
        assert_eq!(captures(&mut schedule, now), vec![wid(5)], "one back, one out");
    }

    /// One listing serves every window that may start: a fresh listing costs about 83ms, against 26ms
    /// for the capture after it.
    #[test]
    fn everything_that_may_start_is_listed_together() {
        let now = Instant::now();
        let mut schedule = Schedule::<()>::default();
        schedule.ask(ask(Priority::Warm, &[1, 2, 3, 4, 5, 6]), settled_by(now));
        assert_eq!(listed_windows(schedule.next(now, nothing_held)).len(), 6);
    }

    /// The most urgent capture takes the next free slot, however long the rest have waited.
    #[test]
    fn a_focus_capture_is_never_queued_behind_warms() {
        let now = Instant::now();
        let mut schedule = Schedule::<()>::default();
        schedule.ask(ask(Priority::Warm, &[1, 2, 3, 4, 5, 6]), settled_by(now));
        assert_eq!(captures(&mut schedule, now).len(), MAX_CONCURRENT);
        schedule.ask(ask(Priority::Focus, &[7]), settled_by(now));
        assert!(schedule.captured(wid(1)).is_some());
        assert_eq!(captures(&mut schedule, now), vec![wid(7)]);
    }

    #[test]
    fn a_window_the_listing_did_not_find_can_be_asked_for_again() {
        let now = Instant::now();
        let mut schedule = Schedule::<()>::default();
        schedule.ask(ask(Priority::Warm, &[1]), settled_by(now));
        let _ = schedule.next(now, nothing_held);
        schedule.listed(wid(1), None);
        assert!(schedule.owed().next().is_none());
        schedule.ask(ask(Priority::Warm, &[1]), settled_by(now));
        assert_eq!(listed_windows(schedule.next(now, nothing_held)), vec![wid(1)]);
    }

    /// A closed window is captured no further, and a capture already out for it is not kept.
    #[test]
    fn a_forgotten_window_is_captured_no_further() {
        let now = Instant::now();
        let mut schedule = Schedule::<()>::default();
        schedule.hold(Hold::Transition, now);
        schedule.ask(ask(Priority::Warm, &[1]), settled_by(now));
        schedule.forget(wid(1));
        schedule.hold(Hold::Released, now);
        assert!(captures(&mut schedule, now).is_empty(), "waiting");

        schedule.ask(ask(Priority::Warm, &[2, 3]), settled_by(now));
        let _ = schedule.next(now, nothing_held);
        schedule.listed(wid(2), Some(()));
        schedule.forget(wid(2));
        schedule.forget(wid(3));
        schedule.listed(wid(3), Some(()));
        assert!(captures(&mut schedule, now).is_empty(), "listing and listed");

        schedule.ask(ask(Priority::Warm, &[4]), settled_by(now));
        assert_eq!(captures(&mut schedule, now), vec![wid(4)]);
        schedule.forget(wid(4));
        assert!(schedule.captured(wid(4)).is_none(), "out");
        assert!(schedule.owed().next().is_none());
    }

    /// The reported case: with a second display attached, every flight on the other one changed the
    /// display, and the change dropped every window capture in flight, so windows went on flying with
    /// no picture. Only the desktop render belongs to a display.
    #[test]
    fn a_display_change_keeps_the_window_captures_out() {
        let now = Instant::now();
        let mut schedule = Schedule::<()>::default();
        schedule.set_display(built_in(), false, now);
        schedule.ask(ask(Priority::Warm, &[1]), settled_by(now));
        assert_eq!(captures(&mut schedule, now), vec![wid(1)]);
        assert!(schedule.set_display(external(), false, now));
        assert!(schedule.captured(wid(1)).is_some(), "kept");
        let rescaled = Display { scale: 1.0, ..external() };
        schedule.ask(ask(Priority::Warm, &[2]), settled_by(now));
        assert_eq!(captures(&mut schedule, now), vec![wid(2)]);
        assert!(schedule.set_display(rescaled, false, now), "a scale change is a display change");
        assert!(schedule.captured(wid(2)).is_some(), "kept through a scale change too");
        assert!(!schedule.set_display(rescaled, false, now), "the same display again");
    }

    /// A display change used to ask for its render before invalidating the one in flight, so the
    /// render it asked for was thrown away when it landed and every flight after a display change
    /// paid for a composite on the main thread. The render of the display left is refused, and the one
    /// asked for after the change is the new display's, and kept.
    #[test]
    fn a_display_change_renders_the_new_displays_desktop_after_the_old_one_is_refused() {
        let now = Instant::now();
        let mut schedule = Schedule::<()>::default();
        schedule.set_display(built_in(), false, now);
        assert!(matches!(schedule.next(now, nothing_held), Next::Desktop(d) if d == built_in()));
        schedule.set_display(external(), false, now);
        assert!(
            matches!(schedule.next(now, nothing_held), Next::Idle),
            "one render out at a time"
        );
        assert!(!schedule.desktop_rendered(built_in(), Some(now)), "the display left");
        assert!(matches!(schedule.next(now, nothing_held), Next::Desktop(d) if d == external()));
        assert!(schedule.desktop_rendered(external(), Some(now)));
        assert!(matches!(schedule.next(now, nothing_held), Next::Idle));
    }

    /// The render waits out a transition like any background work, and a release renders again only
    /// what is missing or stale.
    #[test]
    fn the_desktop_is_rendered_again_at_release_only_when_missing_or_stale() {
        let now = Instant::now();
        let mut schedule = Schedule::<()>::default();
        schedule.hold(Hold::Transition, now);
        schedule.set_display(built_in(), false, now);
        assert!(matches!(schedule.next(now, nothing_held), Next::Idle), "held");
        schedule.hold(Hold::Released, now);
        assert!(matches!(schedule.next(now, nothing_held), Next::Desktop(_)));
        assert!(schedule.desktop_rendered(built_in(), Some(now)));

        schedule.hold(Hold::Transition, now);
        schedule.hold(Hold::Released, now + Duration::from_millis(500));
        assert!(matches!(schedule.next(now, nothing_held), Next::Idle), "in hand");
        schedule.hold(Hold::Transition, now);
        schedule.hold(Hold::Released, now + Duration::from_secs(3));
        assert!(matches!(schedule.next(now, nothing_held), Next::Desktop(_)), "stale");
        assert!(!schedule.desktop_rendered(built_in(), None), "failed");
        schedule.hold(Hold::Transition, now);
        schedule.hold(Hold::Released, now + Duration::from_secs(3));
        assert!(
            matches!(schedule.next(now, nothing_held), Next::Desktop(_)),
            "the render held is still stale, so it is tried again"
        );
    }

    /// The bar is read off the framebuffer, so it is pictured `BAR_REFRESH_DELAY` after the overlay
    /// lifts and never while it is up: a transition beginning in between waits for its own lift.
    #[test]
    fn the_bar_is_pictured_after_the_overlay_lifts_and_never_while_it_is_up() {
        let now = Instant::now();
        let mut schedule = Schedule::<()>::default();
        schedule.set_display(built_in(), true, now);
        assert!(matches!(schedule.next(now, nothing_held), Next::Desktop(_)));
        assert!(matches!(
            schedule.next(now, nothing_held),
            Next::WaitUntil(due) if due == now + BAR_REFRESH_DELAY
        ));

        schedule.hold(Hold::Transition, now);
        let after = now + BAR_REFRESH_DELAY;
        assert!(matches!(schedule.next(after, nothing_held), Next::Idle), "overlay up");
        schedule.set_display(external(), true, after);
        assert!(
            !matches!(schedule.next(after + BAR_REFRESH_DELAY, nothing_held), Next::Bar(_)),
            "a display change under the overlay does not picture it either"
        );

        let lift = after + Duration::from_millis(100);
        schedule.hold(Hold::Settling, lift);
        assert!(matches!(schedule.next(lift, nothing_held), Next::WaitUntil(due) if due == lift + BAR_REFRESH_DELAY));
        assert!(matches!(
            schedule.next(lift + BAR_REFRESH_DELAY, nothing_held),
            Next::Bar(d) if d == external()
        ));
        assert!(matches!(schedule.next(lift + BAR_REFRESH_DELAY, nothing_held), Next::Idle));
    }

    fn tag() -> Tag {
        Tag {
            client: crate::pictures::domain::watch::Client(1),
            generation: 1,
        }
    }

    fn landing(window: u32, end: CGRect, until: Instant) -> Watch {
        Watch::Landing {
            tag: tag(),
            windows: vec![(target(window, 859.0), end)],
            behind: Vec::new(),
            slack: crate::pictures::domain::watch::END_SLACK,
            settle: crate::pictures::domain::watch::Settle::AsSoonAsThere,
            until,
        }
    }

    fn end() -> CGRect { CGRect::new(CGPoint::new(4.0, 32.0), CGSize::new(859.0, 1081.0)) }

    fn no_print(_: WindowId) -> Option<Vec<u8>> { None }

    /// What a transition waits on through SkyLight goes before any ScreenCaptureKit call is issued:
    /// queued ScreenCaptureKit work slows every SkyLight call behind it.
    #[test]
    fn skylight_work_a_transition_waits_on_goes_before_any_screencapturekit_call() {
        let now = Instant::now();
        let mut schedule = Schedule::<()>::default();
        schedule.set_display(built_in(), false, now);
        schedule.ask(ask(Priority::Focus, &[1]), now);
        schedule.watch(WatchId(1), landing(2, end(), now + Duration::from_millis(150)), now, &no_print);
        assert!(matches!(schedule.next(now, nothing_held), Next::Sky(_)));
        let _ = schedule.seen(now, &[]);
        assert_eq!(listed_windows(schedule.next(now, nothing_held)), vec![wid(1)]);
    }

    /// A window that did not land is reported before anything else is done, and the schedule wakes for
    /// the watch's next look.
    #[test]
    fn a_lost_window_is_reported_first_and_the_schedule_wakes_for_its_watches() {
        let now = Instant::now();
        let mut schedule = Schedule::<()>::default();
        schedule.watch(WatchId(1), landing(2, end(), now + Duration::from_millis(150)), now, &no_print);
        assert!(matches!(schedule.next(now, nothing_held), Next::Sky(_)));
        let _ = schedule.seen(now, &[]);
        assert!(matches!(
            schedule.next(now, nothing_held),
            Next::WaitUntil(at) if at == now + crate::pictures::domain::watch::LANDING_POLL
        ));
        let late = now + Duration::from_millis(150);
        assert!(matches!(
            schedule.next(late, nothing_held),
            Next::Lost(lost) if lost == vec![(tag(), wid(2))]
        ));
    }

    /// A focus target cut by the display's edge is owed one ScreenCaptureKit capture, which starts
    /// through the transition's hold and comes back owed to the watch's client as its focus change.
    #[test]
    fn a_clipped_focus_end_takes_one_capture_through_the_hold() {
        use crate::pictures::domain::watch::{CHANGE_EVERY, CHANGE_TRIES, Hint, Prints, Seen};
        let now = Instant::now();
        let mut schedule = Schedule::<()>::default();
        schedule.set_display(built_in(), false, now);
        let _ = schedule.next(now, nothing_held);
        let _ = schedule.desktop_rendered(built_in(), Some(now));
        schedule.hold(Hold::Transition, now);
        schedule.hint(Hint::Focused(wid(1)), now);
        let clipped = CGRect::new(CGPoint::new(1300.0, 32.0), CGSize::new(859.0, 1081.0));
        schedule.watch(
            WatchId(1),
            Watch::Change {
                tag: tag(),
                window: target(1, 859.0),
                end: clipped,
                behind: Vec::new(),
                every: CHANGE_EVERY,
                tries: CHANGE_TRIES,
            },
            now,
            &no_print,
        );
        assert!(matches!(schedule.next(now, nothing_held), Next::Sky(_)));
        let outcome = schedule.seen(
            now,
            &[Seen {
                window: wid(1),
                frame: Some(clipped),
                prints: None::<Prints>,
            }],
        );
        assert!(outcome.clipped.is_empty(), "taken over by the schedule");
        assert_eq!(captures(&mut schedule, now), vec![wid(1)], "through the hold");
        assert_eq!(schedule.captured(wid(1)), Some(vec![(tag(), clipped)]));
    }

    /// A capture of the window already out began before its focus: the clipped end's capture follows
    /// it, a job of its own.
    #[test]
    fn a_clipped_focus_end_is_captured_after_a_capture_already_out() {
        let now = Instant::now();
        let mut schedule = Schedule::<()>::default();
        schedule.ask(ask(Priority::Focus, &[1]), now);
        assert_eq!(captures(&mut schedule, now), vec![wid(1)]);
        schedule.ask_clipped(tag(), target(1, 859.0), end(), now);
        assert_eq!(schedule.captured(wid(1)), Some(Vec::new()), "the one out owes no one");
        assert_eq!(captures(&mut schedule, now), vec![wid(1)]);
        assert_eq!(schedule.captured(wid(1)), Some(vec![(tag(), end())]));
        schedule.ask_clipped(tag(), target(1, 859.0), end(), now);
        schedule.ask(ask(Priority::Focus, &[1]), now);
        schedule.forget(wid(1));
        assert!(schedule.owed().next().is_none(), "forgotten, every job of it");
    }

    #[test]
    fn no_bar_is_pictured_while_rini_draws_its_own() {
        let now = Instant::now();
        let mut schedule = Schedule::<()>::default();
        schedule.set_display(built_in(), false, now);
        let _ = schedule.next(now, nothing_held);
        schedule.hold(Hold::Settling, now);
        assert!(matches!(schedule.next(now + BAR_REFRESH_DELAY, nothing_held), Next::Idle));
    }
}
