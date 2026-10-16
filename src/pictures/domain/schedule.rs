//! The capture schedule: which call the pictures service makes next.
//!
//! Pure. Asks, watches, hints, holds, refreshes, display changes, handover checks and what each call
//! brought back go in, with `now` wherever time matters; the next call comes out. The SkyLight half is
//! windows on screen captured as a transition asks and watches, `crate::pictures::domain::watch`, and
//! windows pictured afresh at rest, `crate::pictures::domain::refresh`; this orders it ahead of the
//! ScreenCaptureKit half, and a handover check ahead of both. `crate::pictures::platform::service`
//! makes the calls, on its own thread. The rules: "The schedule" and "The watches" in
//! `src/pictures/docs/README.md`; each in full and why it holds, the same sections in
//! `src/pictures/docs/capture-research.md`.

use std::time::{Duration, Instant};

use rini_core::ids::{WindowId, WindowServerId};
use rini_geometry::space::{ScreenRect, ScreenSize};

use crate::pictures::domain::coverage::{Coverage, needs_capture, picture_is_stale};
use crate::pictures::domain::handover::{AFTER_LIFT, HandoverCheck};
use crate::pictures::domain::refresh::Refreshes;
use crate::pictures::domain::target::PictureTarget;
use crate::pictures::domain::watch::{
    CapturedElsewhere, Hint, OnScreen, Outcome, Seen, Set, Tag, Watch, WatchId, Watches,
};

/// How urgently windows are wanted, most urgent first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Priority {
    /// A window a flight holds for, repainting where it ends cut by the display's edge, which
    /// SkyLight cannot picture whole: the motion waits for it.
    Repaint,
    /// A focus target ending clipped by the display's edge, which SkyLight cannot picture whole.
    Focus,
    /// A window refreshed at rest that the window server does not have wholly on the display, or that
    /// SkyLight never captured, whose turned face keeps the size the cache holds it at.
    Background,
    /// Windows named for a later transition or the switcher: skipped where the picture held fits and is
    /// fresh.
    Warm,
}

impl Priority {
    /// A repaint or a focus capture starts through a hold, and at once: the transition holding the
    /// service is the one waiting for it.
    fn wanted_now(self) -> bool { matches!(self, Self::Repaint | Self::Focus) }
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
    /// A transition runs: of ScreenCaptureKit's work only a repaint or a focus capture starts, every
    /// refresh is cancelled, and the bar is not pictured, since a picture of it is read off the
    /// framebuffer the overlay covers. A handover check under way is dropped, with the overlay's
    /// picture it holds: the screen it would take may show the overlay again.
    Transition,
    /// The overlay has lifted and the transition is settling: the bar is pictured again, and the rest
    /// still waits.
    Settling,
    /// Nothing holds the service: what waited starts, most urgent first, and each refresh a hold
    /// cancelled is asked again.
    Released,
}

/// The display the overlay flies on, as far as a picture of it is concerned.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Display {
    pub id: u32,
    /// The whole display.
    pub frame: ScreenRect,
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
pub fn wants_warm(held: Option<Held>, size: ScreenSize, now: Instant) -> bool {
    needs_capture(held.map(|held| held.coverage), size)
        || held.is_some_and(|held| picture_is_stale(now.saturating_duration_since(held.taken)))
}

/// How long a background or warm ask waits before it is listed, and a refresh, with every refresh
/// already owed, before its next look. The reactor sends a pass's warm straight here just after the
/// request for its flight, and the flight engine a click's refresh just before the request for the
/// flight the click starts, so either usually arrives before that flight's hold, which the engine sends
/// as it takes the request; started at once, it would put ScreenCaptureKit to work at the flight's
/// frame zero, or a SkyLight call ahead of it. A flight whose request waits behind the engine's own
/// work on the `transitions` thread, such as a compose, holds later than this, and so does one a press
/// starts with no face turned: what is listed meanwhile is listed again at the release (`hold`), a
/// refresh is cancelled by the hold and asked again at the release, and the service checks for a
/// transition asked before each SkyLight call of a refresh, so at most the one call already out is
/// ahead of frame zero.
pub const SETTLE_BEFORE_STARTING: Duration = Duration::from_millis(50);

/// Concurrent ScreenCaptureKit captures. It serialises internally, so wall clock stops improving past
/// four. See "Capture cost" in `src/pictures/docs/capture-research.md`.
pub const MAX_CONCURRENT: usize = 4;

/// How long after the overlay lifts, or the display changes, the bar is pictured: a bar composite is
/// too slow (31ms median) to pay per switch, so a burst of switches pays it once, and the compositor
/// has dropped the hidden overlay from the framebuffer by then.
pub const BAR_REFRESH_DELAY: Duration = Duration::from_millis(250);

/// How many new windows one pass captures synchronously at spawn; the rest take a reservation.
/// See "A window that opens" in `src/transitions/docs/coordination.md`.
pub const MAX_SYNC_ENTRANCE_CAPTURES: usize = 4;

/// The service's next call. `P` is what the platform prepared for capturing a window its listing found,
/// `O` the overlay's own picture a handover check holds until the screen is taken.
#[derive(Debug)]
pub enum Next<P, O> {
    /// Tell these clients their windows did not land by the deadline they gave.
    Lost(Vec<(Tag, WindowId)>),
    /// Read these windows' server frames, capture those where they are wanted through SkyLight, set by
    /// set, and say what was seen (`Schedule::seen`).
    Sky(Vec<Set>),
    /// The same for a refresh at rest, every window due in the one set, and say what was seen
    /// (`Schedule::refreshed`). Its calls stop short once a transition is asked: what is not called
    /// is left out of what is seen.
    Refresh(Set),
    /// Find these windows in one ScreenCaptureKit listing, and prepare a capture of each.
    List(Vec<PictureTarget>),
    /// Capture a window the listing found.
    Capture { target: PictureTarget, prepared: P },
    /// Render the desktop of this display.
    Desktop(Display),
    /// Picture the bar of this display.
    Bar(Display),
    /// Picture this overlay window alone, for the handover check under way, and say when it was taken
    /// (`Schedule::overlay_taken`).
    Overlay(WindowServerId),
    /// Picture the screen, and compare it over each window with the overlay's picture.
    Screen(HandoverCheck, O),
    /// Nothing before then, unless something arrives.
    WaitUntil(Instant),
    /// Nothing until something arrives.
    Idle,
}

/// Every capture owed and under way, and the holds over them. `P` and `O` are the platform's, as in
/// [`Next`]; `C` a capture a refresh holds until its window's face settles.
pub struct Schedule<P, O, C> {
    hold: Hold,
    watches: Watches,
    refreshes: Refreshes<C>,
    jobs: Vec<Job<P>>,
    asked: u64,
    display: Option<Display>,
    picture_bar: bool,
    desktop: Desktop,
    bar_due: Option<Instant>,
    check: Option<Checking<O>>,
}

/// A handover check under way: its overlay's own picture the next call, being taken, then held until
/// the screen is taken `AFTER_LIFT` after it.
enum Checking<O> {
    Overlay(HandoverCheck),
    Taking(HandoverCheck),
    Screen {
        check: HandoverCheck,
        overlay: O,
        due: Instant,
    },
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
    deliver: Vec<(Tag, ScreenRect)>,
    /// A repaint watch judges what comes back (`Schedule::captured_elsewhere`).
    settles: bool,
}

/// Who a capture that came back is owed to.
#[derive(Debug, Clone, PartialEq)]
pub struct Owed {
    /// Clients owed it as a focus change, with the server frame each asked at.
    pub focus: Vec<(Tag, ScreenRect)>,
    /// A repaint watch judges it (`Schedule::captured_elsewhere`).
    pub settles: bool,
}

impl<P> Job<P> {
    /// When a waiting job may be listed: at once for what a transition waits on, once settled for
    /// the rest.
    fn listable_from(&self) -> Instant {
        if self.priority.wanted_now() {
            self.asked_at
        } else {
            self.asked_at + SETTLE_BEFORE_STARTING
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

impl<P, O, C> Default for Schedule<P, O, C> {
    fn default() -> Self {
        Self {
            hold: Hold::Released,
            watches: Watches::default(),
            refreshes: Refreshes::default(),
            jobs: Vec::new(),
            asked: 0,
            display: None,
            picture_bar: false,
            desktop: Desktop::default(),
            bar_due: None,
            check: None,
        }
    }
}

impl<P, O, C> Schedule<P, O, C> {
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
                    settles: false,
                }),
            }
        }
    }

    /// A capture listed before a hold and unable to start through it is listed again at the release:
    /// the transition may resize its window, and the buffer the listing prepared would not fit it. A
    /// hold cancels every refresh, and the release asks again from `SETTLE_BEFORE_STARTING` on.
    pub fn hold(&mut self, hold: Hold, now: Instant) {
        self.hold = hold;
        if hold != Hold::Released {
            self.refreshes.cancel();
            for job in &mut self.jobs {
                if matches!(job.stage, Stage::Listed(_)) && !job.priority.wanted_now() {
                    job.stage = Stage::Waiting;
                }
            }
        }
        match hold {
            Hold::Transition => {
                self.bar_due = None;
                self.check = None;
            }
            Hold::Settling => self.arm_bar(now),
            Hold::Released => {
                self.refreshes.release(now + SETTLE_BEFORE_STARTING);
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

    /// A lift to measure, in place of any check under way: its overlay's own picture is the next call.
    pub fn check_handover(&mut self, check: HandoverCheck) {
        self.check = Some(Checking::Overlay(check))
    }

    /// The overlay's own picture of the check under way, taken at `at`, or `None` when it was not: the
    /// screen is due with it `AFTER_LIFT` later, or never. The check's tag, to tell its client.
    pub fn overlay_taken(&mut self, at: Instant, picture: Option<O>) -> Option<Tag> {
        let Some(Checking::Taking(check)) = self.check.take() else {
            return None;
        };
        let tag = check.tag;
        if let Some(overlay) = picture {
            self.check = Some(Checking::Screen {
                check,
                overlay,
                due: at + AFTER_LIFT,
            });
        }
        Some(tag)
    }

    /// Windows on screen, wanted now: see [`OnScreen`].
    pub fn ask_on_screen(&mut self, ask: OnScreen) { self.watches.ask(ask) }

    /// Windows whose face has just turned, pictured afresh at rest (see [`Refreshes`]) from
    /// `SETTLE_BEFORE_STARTING` on, every refresh already owed held until then too. Asked while held,
    /// they are cancelled as they arrive, and asked again at the release.
    pub fn refresh(&mut self, targets: Vec<PictureTarget>, now: Instant) {
        self.refreshes.ask(targets, now, now + SETTLE_BEFORE_STARTING);
        if self.held() {
            self.refreshes.cancel();
        }
    }

    pub fn watch(&mut self, id: WatchId, watch: Watch, now: Instant) {
        self.watches.watch(id, watch, now)
    }

    pub fn retarget(&mut self, id: WatchId, watch: Watch, now: Instant) {
        self.watches.retarget(id, watch, now)
    }

    pub fn stop(&mut self, id: WatchId) { self.watches.stop(id) }

    pub fn hint(&mut self, hint: Hint, now: Instant) { self.watches.hint(hint, now) }

    /// What the window server and the captures said of the windows `Next::Sky` named. A window a
    /// changing or a repainting watch found cut by the display's edge is owed a ScreenCaptureKit
    /// capture, which starts through any hold.
    pub fn seen(&mut self, now: Instant, seen: &[Seen]) -> Outcome {
        let display = self.display.map(|display| display.frame);
        let mut outcome = self.watches.seen(now, seen, display);
        for (tag, target, frame) in std::mem::take(&mut outcome.clipped) {
            self.ask_clipped(tag, target, frame, now);
        }
        self.ask_elsewhere(&mut outcome, now);
        outcome
    }

    /// What came of the ScreenCaptureKit capture of `window` a repaint watch was owed (`Owed::settles`),
    /// as the watch judges it (`Watches::captured_elsewhere`): one more is owed until it settles.
    pub fn captured_elsewhere(
        &mut self,
        window: WindowId,
        capture: CapturedElsewhere,
        now: Instant,
    ) -> Outcome {
        let mut outcome = self.watches.captured_elsewhere(window, capture);
        self.ask_elsewhere(&mut outcome, now);
        outcome
    }

    /// One capture of each window `outcome` owes a repaint watch, at `Priority::Repaint`. A listing or
    /// a capture of the window already out began before it was there, so it is followed by one of its
    /// own.
    fn ask_elsewhere(&mut self, outcome: &mut Outcome, now: Instant) {
        for target in std::mem::take(&mut outcome.elsewhere) {
            self.asked += 1;
            let waiting = self.jobs.iter_mut().find(|job| {
                job.target.window == target.window && matches!(job.stage, Stage::Waiting)
            });
            match waiting {
                Some(job) => {
                    job.target = target;
                    job.priority = job.priority.min(Priority::Repaint);
                    job.forgotten = false;
                    job.settles = true;
                }
                None => self.jobs.push(Job {
                    target,
                    priority: Priority::Repaint,
                    order: self.asked,
                    asked_at: now,
                    stage: Stage::Waiting,
                    forgotten: false,
                    deliver: Vec::new(),
                    settles: true,
                }),
            }
        }
    }

    /// What the window server and the captures said of the windows `Next::Refresh` named, with the
    /// capture of each the cache would take: the capture each window whose face settled is pictured
    /// with, once (see [`Refreshes::seen`]). One a refresh cannot capture through SkyLight is owed a
    /// ScreenCaptureKit capture at background priority.
    pub fn refreshed(
        &mut self,
        now: Instant,
        seen: &[Seen],
        captures: Vec<(WindowId, C)>,
    ) -> Vec<(WindowId, C)> {
        let display = self.display.map(|display| display.frame);
        let refreshed = self.refreshes.seen(now, seen, captures, display);
        if !refreshed.elsewhere.is_empty() {
            self.ask(
                Ask {
                    priority: Priority::Background,
                    targets: refreshed.elsewhere,
                },
                now,
            );
        }
        refreshed.settled
    }

    /// One capture of `target` for `tag`'s focus change. A capture already out began before the focus,
    /// so it is followed by one of its own.
    fn ask_clipped(&mut self, tag: Tag, target: PictureTarget, frame: ScreenRect, now: Instant) {
        self.asked += 1;
        let waiting = self.jobs.iter_mut().find(|job| {
            job.target.window == target.window
                && matches!(job.stage, Stage::Waiting | Stage::Listed(_))
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
                settles: false,
            }),
        }
    }

    /// A window that is gone: nothing more is captured of it, and nothing already out is kept.
    pub fn forget(&mut self, window: WindowId) {
        self.watches.forget(window);
        self.refreshes.forget(window);
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

    /// A capture of `window` came back. `None` when what it brought may not be kept; else who it is
    /// owed to.
    pub fn captured(&mut self, window: WindowId) -> Option<Owed> {
        let at = self.position(window, |stage| matches!(stage, Stage::Capturing))?;
        let job = self.jobs.remove(at);
        (!job.forgotten).then_some(Owed {
            focus: job.deliver,
            settles: job.settles,
        })
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

    /// Whether a bar under the overlay is pictured and drawn over transitions: off while rini draws
    /// its own bar above the overlay, which leaves nothing there to picture.
    pub fn picture_bar(&self) -> bool { self.picture_bar }

    /// Whether a transition holds the service.
    pub fn held(&self) -> bool { self.hold != Hold::Released }

    /// Every window owed a capture or with one out, but for those forgotten.
    #[cfg(test)]
    pub fn owed(&self) -> impl Iterator<Item = WindowId> + '_ {
        self.jobs
            .iter()
            .filter(|job| !job.forgotten)
            .map(|job| job.target.window)
            .chain(self.refreshes.owed())
    }

    /// The next call to make. `held` is what the cache holds worth drawing of a window, for the warm
    /// filter and a refresh asked again.
    pub fn next(&mut self, now: Instant, held: impl Fn(WindowId) -> Option<Held>) -> Next<P, O> {
        // A measured lift holds its hide for the overlay's own picture.
        match self.check.take() {
            Some(Checking::Overlay(check)) => {
                let overlay = check.overlay;
                self.check = Some(Checking::Taking(check));
                return Next::Overlay(overlay);
            }
            Some(Checking::Screen { check, overlay, due }) if due <= now => {
                return Next::Screen(check, overlay);
            }
            other => self.check = other,
        }
        // What a transition waits on starts before any ScreenCaptureKit call is issued, and so, at
        // rest, does a refresh: queued ScreenCaptureKit work slows every SkyLight call behind it.
        let lost = self.watches.expire(now);
        if !lost.is_empty() {
            return Next::Lost(lost);
        }
        if let Some(sets) = self.watches.next(now) {
            return Next::Sky(sets);
        }
        if let Some(set) = self.refreshes.next(now, |window| held(window).map(|held| held.taken)) {
            return Next::Refresh(set);
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

        let capturing =
            self.jobs.iter().filter(|job| matches!(job.stage, Stage::Capturing)).count();
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
        let screen = match &self.check {
            Some(Checking::Screen { due, .. }) => Some(*due),
            _ => None,
        };
        let refreshing = self.refreshes.wake();
        let bar = self.bar_due.filter(|_| self.display.is_some());
        match bar
            .into_iter()
            .chain(settling)
            .chain(watching)
            .chain(refreshing)
            .chain(screen)
            .min()
        {
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
    use std::rc::Rc;

    use rini_core::ids::WindowServerId;
    use rini_geometry::space::ScreenPoint;

    use super::*;

    fn wid(idx: u32) -> WindowId { WindowId::new(1, idx) }

    fn target(idx: u32, width: f64) -> PictureTarget {
        PictureTarget {
            window: wid(idx),
            server_id: WindowServerId::new(idx),
            size: ScreenSize::new(width, 1081.0),
        }
    }

    fn ask(priority: Priority, windows: &[u32]) -> Ask {
        Ask {
            priority,
            targets: windows.iter().map(|idx| target(*idx, 859.0)).collect(),
        }
    }

    fn rect(x: f64, y: f64, w: f64, h: f64) -> ScreenRect {
        ScreenRect::new(ScreenPoint::new(x, y), ScreenSize::new(w, h))
    }

    fn built_in() -> Display {
        Display {
            id: 1,
            frame: rect(0.0, 0.0, 1728.0, 1117.0),
            scale: 2.0,
        }
    }

    fn external() -> Display {
        Display {
            id: 2,
            frame: rect(-670.0, -1692.0, 3008.0, 1692.0),
            scale: 2.0,
        }
    }

    fn nothing_held(_: WindowId) -> Option<Held> { None }

    /// Long enough before `now` for a background or warm ask to have settled.
    fn settled_by(now: Instant) -> Instant { now - SETTLE_BEFORE_STARTING }

    fn listed_windows(next: Next<(), ()>) -> Vec<WindowId> {
        match next {
            Next::List(targets) => targets.into_iter().map(|t| t.window).collect(),
            other => panic!("expected a listing, got {other:?}"),
        }
    }

    /// Lists and prepares everything that may start, then hands out the captures one by one.
    fn captures(schedule: &mut Schedule<(), (), ()>, now: Instant) -> Vec<WindowId> {
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

    /// From the begin to the release nothing of ScreenCaptureKit's starts but what the transition waits
    /// for: the one capture of a focus target clipped by the display's edge. Its own captures are
    /// SkyLight's and never come here.
    #[test]
    fn held_work_waits_and_only_a_focus_capture_starts_through_a_hold() {
        for hold in [Hold::Transition, Hold::Settling] {
            let now = Instant::now();
            let mut schedule = Schedule::<(), (), ()>::default();
            schedule.hold(hold, now);
            schedule.ask(ask(Priority::Warm, &[1]), settled_by(now));
            schedule.ask(ask(Priority::Background, &[2]), settled_by(now));
            schedule.ask(ask(Priority::Focus, &[3]), settled_by(now));
            assert_eq!(captures(&mut schedule, now), vec![wid(3)], "{hold:?}");

            schedule.hold(Hold::Released, now);
            assert_eq!(
                captures(&mut schedule, now),
                vec![wid(2), wid(1)],
                "released, in order"
            );
        }
    }

    /// The full table, hold by priority: what starts.
    #[test]
    fn what_starts_under_each_hold() {
        const PRIORITIES: [Priority; 4] = [
            Priority::Repaint,
            Priority::Focus,
            Priority::Background,
            Priority::Warm,
        ];
        let mut started = 0usize;
        for hold in [Hold::Released, Hold::Transition, Hold::Settling] {
            for priority in PRIORITIES {
                let now = Instant::now();
                let mut schedule = Schedule::<(), (), ()>::default();
                schedule.hold(hold, now);
                schedule.ask(ask(priority, &[1]), settled_by(now));
                let starts = !captures(&mut schedule, now).is_empty();
                let expected = hold == Hold::Released
                    || matches!(priority, Priority::Repaint | Priority::Focus);
                assert_eq!(starts, expected, "{hold:?}/{priority:?}");
                started += starts as usize;
            }
        }
        // Released 4, each hold 2.
        assert_eq!(started, 8);
    }

    /// A pass's warm usually reaches the service before the hold of the flight that pass started, so
    /// it waits long enough for that hold to arrive, and then out the flight.
    #[test]
    fn a_warm_asked_just_before_its_flight_holds_waits_out_the_flight() {
        let now = Instant::now();
        let mut schedule = Schedule::<(), (), ()>::default();
        schedule.ask(ask(Priority::Warm, &[1]), now);
        assert!(matches!(
            schedule.next(now, nothing_held),
            Next::WaitUntil(at) if at == now + SETTLE_BEFORE_STARTING
        ));
        schedule.hold(Hold::Transition, now + Duration::from_millis(10));
        assert!(
            matches!(
                schedule.next(now + SETTLE_BEFORE_STARTING, nothing_held),
                Next::Idle
            ),
            "held"
        );
        let release = now + Duration::from_millis(500);
        schedule.hold(Hold::Released, release);
        assert_eq!(listed_windows(schedule.next(release, nothing_held)), vec![wid(
            1
        )]);
    }

    #[test]
    fn a_focus_capture_is_listed_at_once() {
        let now = Instant::now();
        let mut schedule = Schedule::<(), (), ()>::default();
        schedule.ask(ask(Priority::Focus, &[1]), now);
        assert_eq!(listed_windows(schedule.next(now, nothing_held)), vec![wid(1)]);
    }

    /// A capture listed before the transition does not start inside it either: ScreenCaptureKit work
    /// queued at a focus change slows every step of it. It is listed again at the release.
    #[test]
    fn a_capture_listed_before_the_transition_waits_for_its_release() {
        let now = Instant::now();
        let mut schedule = Schedule::<(), (), ()>::default();
        schedule.ask(ask(Priority::Warm, &[1, 2, 3, 4, 5, 6]), settled_by(now));
        let Next::List(targets) = schedule.next(now, nothing_held) else {
            panic!("a listing")
        };
        for target in targets {
            schedule.listed(target.window, Some(()));
        }
        schedule.hold(Hold::Transition, now);
        assert!(matches!(schedule.next(now, nothing_held), Next::Idle));
        schedule.hold(Hold::Released, now);
        assert_eq!(captures(&mut schedule, now).len(), MAX_CONCURRENT);
    }

    /// A warm listed just before a flight sits through it with a buffer sized at the listing: kept,
    /// a window the flight resized would be captured into the old size at the release, and the warm
    /// the flight asks for it folded into that stale job. Held, it is listed again.
    #[test]
    fn a_capture_listed_before_a_hold_is_listed_again_at_the_release() {
        let now = Instant::now();
        let mut schedule = Schedule::<(), (), ()>::default();
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
        let Next::List(targets) = schedule.next(now, nothing_held) else {
            panic!("a listing")
        };
        let asked: Vec<(WindowId, f64)> =
            targets.iter().map(|t| (t.window, t.size.width)).collect();
        assert_eq!(
            asked,
            vec![(wid(1), 1147.0)],
            "listed again, at the size asked last"
        );
    }

    /// A listing that returns once the hold is up has prepared a buffer the flight may outgrow too.
    #[test]
    fn a_listing_that_returns_into_a_hold_is_listed_again_at_the_release() {
        let now = Instant::now();
        let mut schedule = Schedule::<(), (), ()>::default();
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
        let mut schedule = Schedule::<(), (), ()>::default();
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
        let Next::List(targets) = schedule.next(now, nothing_held) else {
            panic!("a listing")
        };
        assert_eq!(
            targets.iter().map(|t| (t.window, t.size.width)).collect::<Vec<_>>(),
            vec![(wid(1), 1147.0)]
        );
        assert!(
            matches!(schedule.next(now, nothing_held), Next::Capture { target, .. } if target.window == wid(2))
        );
    }

    /// A warm owed when the focus moves is not asked twice: it takes the focus priority, and starts
    /// through the hold the warm alone would wait out. Whether it waits or is listed already.
    #[test]
    fn a_held_warm_asked_again_at_focus_starts_through_the_hold() {
        let now = Instant::now();
        let mut waiting = Schedule::<(), (), ()>::default();
        waiting.ask(ask(Priority::Warm, &[1]), settled_by(now));
        waiting.hold(Hold::Transition, now);
        waiting.ask(ask(Priority::Focus, &[1]), now);
        assert_eq!(captures(&mut waiting, now), vec![wid(1)], "waiting");

        let mut listed = Schedule::<(), (), ()>::default();
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
        let mut schedule = Schedule::<(), (), ()>::default();
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
        let mut schedule = Schedule::<(), (), ()>::default();
        schedule.hold(Hold::Transition, now);
        schedule.ask(
            Ask {
                priority: Priority::Warm,
                targets: vec![target(1, 859.0), target(2, 859.0)],
            },
            settled_by(now),
        );
        schedule.ask(
            Ask {
                priority: Priority::Warm,
                targets: vec![target(1, 1147.0), target(3, 859.0)],
            },
            settled_by(now),
        );
        schedule.hold(Hold::Released, now);
        let Next::List(targets) = schedule.next(now, nothing_held) else {
            panic!("a listing")
        };
        let asked: Vec<(WindowId, f64)> =
            targets.iter().map(|t| (t.window, t.size.width)).collect();
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
                covered: ScreenSize::new(859.0, 1081.0),
                window: ScreenSize::new(859.0, 1081.0),
            },
            taken: now,
        };
        assert!(!wants_warm(Some(fits), ScreenSize::new(859.0, 1081.0), now));
        assert!(
            wants_warm(Some(fits), ScreenSize::new(1147.0, 1081.0), now),
            "resized"
        );
        assert!(
            wants_warm(None, ScreenSize::new(859.0, 1081.0), now),
            "nothing held"
        );
        let later = now + Duration::from_secs(3);
        assert!(
            wants_warm(Some(fits), ScreenSize::new(859.0, 1081.0), later),
            "stale"
        );

        let mut schedule = Schedule::<(), (), ()>::default();
        schedule.ask(ask(Priority::Warm, &[1, 2]), settled_by(now));
        let held = |window: WindowId| (window == wid(1)).then_some(fits);
        assert_eq!(listed_windows(schedule.next(now, held)), vec![wid(2)]);
        assert!(
            schedule.owed().all(|window| window != wid(1)),
            "dropped, not kept waiting"
        );
    }

    /// A clipped focus end, the display's visible windows and a turned face off the display are
    /// captured whatever is held: none is about a size the cache could already have.
    #[test]
    fn only_a_warm_is_filtered() {
        let now = Instant::now();
        let fits = Held {
            coverage: Coverage {
                covered: ScreenSize::new(859.0, 1081.0),
                window: ScreenSize::new(859.0, 1081.0),
            },
            taken: now,
        };
        for priority in [Priority::Focus, Priority::Background] {
            let mut schedule = Schedule::<(), (), ()>::default();
            schedule.ask(ask(priority, &[1]), settled_by(now));
            assert_eq!(listed_windows(schedule.next(now, |_| Some(fits))), vec![wid(1)]);
        }
    }

    #[test]
    fn a_window_being_captured_is_not_captured_again() {
        let now = Instant::now();
        let mut schedule = Schedule::<(), (), ()>::default();
        schedule.ask(ask(Priority::Warm, &[1]), settled_by(now));
        assert_eq!(captures(&mut schedule, now), vec![wid(1)]);
        schedule.ask(ask(Priority::Focus, &[1]), settled_by(now));
        assert!(
            captures(&mut schedule, now).is_empty(),
            "the one out serves both"
        );
        assert!(schedule.captured(wid(1)).is_some());
        schedule.ask(ask(Priority::Focus, &[1]), settled_by(now));
        assert_eq!(
            captures(&mut schedule, now),
            vec![wid(1)],
            "asked again once it is back"
        );
    }

    #[test]
    fn at_most_four_captures_are_out_at_once() {
        let now = Instant::now();
        let mut schedule = Schedule::<(), (), ()>::default();
        schedule.ask(ask(Priority::Warm, &[1, 2, 3, 4, 5, 6]), settled_by(now));
        assert_eq!(captures(&mut schedule, now), vec![
            wid(1),
            wid(2),
            wid(3),
            wid(4)
        ]);
        assert!(schedule.captured(wid(2)).is_some());
        assert_eq!(captures(&mut schedule, now), vec![wid(5)], "one back, one out");
    }

    /// One listing serves every window that may start: a fresh listing costs about 83ms, against 26ms
    /// for the capture after it.
    #[test]
    fn everything_that_may_start_is_listed_together() {
        let now = Instant::now();
        let mut schedule = Schedule::<(), (), ()>::default();
        schedule.ask(ask(Priority::Warm, &[1, 2, 3, 4, 5, 6]), settled_by(now));
        assert_eq!(listed_windows(schedule.next(now, nothing_held)).len(), 6);
    }

    /// The most urgent capture takes the next free slot, however long the rest have waited.
    #[test]
    fn a_focus_capture_is_never_queued_behind_warms() {
        let now = Instant::now();
        let mut schedule = Schedule::<(), (), ()>::default();
        schedule.ask(ask(Priority::Warm, &[1, 2, 3, 4, 5, 6]), settled_by(now));
        assert_eq!(captures(&mut schedule, now).len(), MAX_CONCURRENT);
        schedule.ask(ask(Priority::Focus, &[7]), settled_by(now));
        assert!(schedule.captured(wid(1)).is_some());
        assert_eq!(captures(&mut schedule, now), vec![wid(7)]);
    }

    #[test]
    fn a_window_the_listing_did_not_find_can_be_asked_for_again() {
        let now = Instant::now();
        let mut schedule = Schedule::<(), (), ()>::default();
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
        let mut schedule = Schedule::<(), (), ()>::default();
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
        let mut schedule = Schedule::<(), (), ()>::default();
        schedule.set_display(built_in(), false, now);
        schedule.ask(ask(Priority::Warm, &[1]), settled_by(now));
        assert_eq!(captures(&mut schedule, now), vec![wid(1)]);
        assert!(schedule.set_display(external(), false, now));
        assert!(schedule.captured(wid(1)).is_some(), "kept");
        let rescaled = Display { scale: 1.0, ..external() };
        schedule.ask(ask(Priority::Warm, &[2]), settled_by(now));
        assert_eq!(captures(&mut schedule, now), vec![wid(2)]);
        assert!(
            schedule.set_display(rescaled, false, now),
            "a scale change is a display change"
        );
        assert!(
            schedule.captured(wid(2)).is_some(),
            "kept through a scale change too"
        );
        assert!(
            !schedule.set_display(rescaled, false, now),
            "the same display again"
        );
    }

    /// A display change invalidates the render in flight before it asks for its own: asked for
    /// first, that render would be thrown away when it landed, and every flight after the change
    /// would pay for a composite on the main thread. The render of the display left is refused, and
    /// the one asked for after the change is the new display's, and kept.
    #[test]
    fn a_display_change_renders_the_new_displays_desktop_after_the_old_one_is_refused() {
        let now = Instant::now();
        let mut schedule = Schedule::<(), (), ()>::default();
        schedule.set_display(built_in(), false, now);
        assert!(matches!(schedule.next(now, nothing_held), Next::Desktop(d) if d == built_in()));
        schedule.set_display(external(), false, now);
        assert!(
            matches!(schedule.next(now, nothing_held), Next::Idle),
            "one render out at a time"
        );
        assert!(
            !schedule.desktop_rendered(built_in(), Some(now)),
            "the display left"
        );
        assert!(matches!(schedule.next(now, nothing_held), Next::Desktop(d) if d == external()));
        assert!(schedule.desktop_rendered(external(), Some(now)));
        assert!(matches!(schedule.next(now, nothing_held), Next::Idle));
    }

    /// The render waits out a transition like any background work, and a release renders again only
    /// what is missing or stale.
    #[test]
    fn the_desktop_is_rendered_again_at_release_only_when_missing_or_stale() {
        let now = Instant::now();
        let mut schedule = Schedule::<(), (), ()>::default();
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
        assert!(
            matches!(schedule.next(now, nothing_held), Next::Desktop(_)),
            "stale"
        );
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
        let mut schedule = Schedule::<(), (), ()>::default();
        schedule.set_display(built_in(), true, now);
        assert!(matches!(schedule.next(now, nothing_held), Next::Desktop(_)));
        assert!(matches!(
            schedule.next(now, nothing_held),
            Next::WaitUntil(due) if due == now + BAR_REFRESH_DELAY
        ));

        schedule.hold(Hold::Transition, now);
        let after = now + BAR_REFRESH_DELAY;
        assert!(
            matches!(schedule.next(after, nothing_held), Next::Idle),
            "overlay up"
        );
        schedule.set_display(external(), true, after);
        assert!(
            !matches!(
                schedule.next(after + BAR_REFRESH_DELAY, nothing_held),
                Next::Bar(_)
            ),
            "a display change under the overlay does not picture it either"
        );

        let lift = after + Duration::from_millis(100);
        schedule.hold(Hold::Settling, lift);
        assert!(
            matches!(schedule.next(lift, nothing_held), Next::WaitUntil(due) if due == lift + BAR_REFRESH_DELAY)
        );
        assert!(matches!(
            schedule.next(lift + BAR_REFRESH_DELAY, nothing_held),
            Next::Bar(d) if d == external()
        ));
        assert!(matches!(
            schedule.next(lift + BAR_REFRESH_DELAY, nothing_held),
            Next::Idle
        ));
    }

    fn tag() -> Tag {
        Tag {
            client: crate::pictures::domain::watch::Client(1),
            generation: 1,
        }
    }

    fn landing(window: u32, end: ScreenRect, until: Instant) -> Watch {
        Watch::Landing {
            tag: tag(),
            windows: vec![(target(window, 859.0), end)],
            behind: Vec::new(),
            slack: crate::pictures::domain::watch::END_SLACK,
            settle: crate::pictures::domain::watch::Settle::AsSoonAsThere,
            until,
        }
    }

    fn end() -> ScreenRect { rect(4.0, 32.0, 859.0, 1081.0) }

    /// What a transition waits on through SkyLight goes before any ScreenCaptureKit call is issued:
    /// queued ScreenCaptureKit work slows every SkyLight call behind it.
    #[test]
    fn skylight_work_a_transition_waits_on_goes_before_any_screencapturekit_call() {
        let now = Instant::now();
        let mut schedule = Schedule::<(), (), ()>::default();
        schedule.set_display(built_in(), false, now);
        schedule.ask(ask(Priority::Focus, &[1]), now);
        schedule.watch(
            WatchId(1),
            landing(2, end(), now + Duration::from_millis(150)),
            now,
        );
        assert!(matches!(schedule.next(now, nothing_held), Next::Sky(_)));
        let _ = schedule.seen(now, &[]);
        assert_eq!(listed_windows(schedule.next(now, nothing_held)), vec![wid(1)]);
    }

    /// A retarget replaces what a watch looks at, and starts one it does not hold; a stopped watch is
    /// looked at no more.
    #[test]
    fn a_watch_is_retargeted_and_stopped_by_its_id() {
        let now = Instant::now();
        let until = now + Duration::from_millis(150);
        let looked = |next: Next<(), ()>| match next {
            Next::Sky(sets) => {
                sets.iter().flat_map(|set| &set.looks).map(|look| look.target.window).collect()
            }
            _ => Vec::new(),
        };
        let mut schedule = Schedule::<(), (), ()>::default();
        schedule.watch(WatchId(1), landing(1, end(), until), now);
        schedule.retarget(WatchId(1), landing(2, end(), until), now);
        schedule.retarget(WatchId(2), landing(3, end(), until), now);
        assert_eq!(looked(schedule.next(now, nothing_held)), vec![wid(2), wid(3)]);
        schedule.stop(WatchId(1));
        schedule.stop(WatchId(2));
        assert!(matches!(schedule.next(now, nothing_held), Next::Idle));
    }

    /// A window that did not land is reported before anything else is done, and the schedule wakes for
    /// the watch's next look.
    #[test]
    fn a_lost_window_is_reported_first_and_the_schedule_wakes_for_its_watches() {
        let now = Instant::now();
        let mut schedule = Schedule::<(), (), ()>::default();
        schedule.watch(
            WatchId(1),
            landing(2, end(), now + Duration::from_millis(150)),
            now,
        );
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
        use crate::pictures::domain::watch::{CHANGE_EVERY, CHANGE_TRIES, Capture, Hint, Seen};
        let now = Instant::now();
        let mut schedule = Schedule::<(), (), ()>::default();
        schedule.set_display(built_in(), false, now);
        let _ = schedule.next(now, nothing_held);
        let _ = schedule.desktop_rendered(built_in(), Some(now));
        schedule.hold(Hold::Transition, now);
        schedule.hint(Hint::Focused(wid(1)), now);
        let clipped = rect(1300.0, 32.0, 859.0, 1081.0);
        schedule.watch(
            WatchId(1),
            Watch::Change {
                tag: tag(),
                window: target(1, 859.0),
                end: clipped,
                losing: None,
                behind: Vec::new(),
                every: CHANGE_EVERY,
                tries: CHANGE_TRIES,
            },
            now,
        );
        assert!(matches!(schedule.next(now, nothing_held), Next::Sky(_)));
        let outcome = schedule.seen(now, &[Seen {
            window: wid(1),
            frame: Some(clipped),
            capture: Capture::Missed,
        }]);
        assert!(outcome.clipped.is_empty(), "taken over by the schedule");
        assert_eq!(captures(&mut schedule, now), vec![wid(1)], "through the hold");
        assert_eq!(
            schedule.captured(wid(1)).map(|owed| owed.focus),
            Some(vec![(tag(), clipped)])
        );
    }

    /// A repainting window ending cut by the display's edge is captured through ScreenCaptureKit,
    /// through the transition's hold, one capture after another as its watch judges each, until two in
    /// a row match and it is delivered as landed.
    #[test]
    fn a_repainting_end_cut_by_the_displays_edge_is_captured_through_the_hold_until_it_settles() {
        use crate::pictures::domain::watch::{Capture, Seen, Settle, Why};
        let now = Instant::now();
        let mut schedule = Schedule::<(), (), ()>::default();
        schedule.set_display(built_in(), false, now);
        let _ = schedule.next(now, nothing_held);
        let _ = schedule.desktop_rendered(built_in(), Some(now));
        schedule.hold(Hold::Transition, now);
        let cut = rect(-285.0, 32.0, 1148.0, 1081.0);
        let landing = Watch::Landing {
            tag: tag(),
            windows: vec![(target(1, 1148.0), cut)],
            behind: Vec::new(),
            slack: 20.0,
            settle: Settle::Repainted,
            until: now + Duration::from_millis(300),
        };
        schedule.watch(WatchId(1), landing, now);
        assert!(matches!(schedule.next(now, nothing_held), Next::Sky(_)));
        let outcome = schedule.seen(now, &[Seen {
            window: wid(1),
            frame: Some(cut),
            capture: Capture::Missed,
        }]);
        assert!(outcome.elsewhere.is_empty(), "taken over by the schedule");
        let rounds = [
            (CapturedElsewhere::Missed, false),
            (CapturedElsewhere::Taken(vec![4]), false),
            (CapturedElsewhere::Taken(vec![4]), true),
        ];
        for (capture, settled) in rounds {
            assert_eq!(captures(&mut schedule, now), vec![wid(1)], "through the hold");
            assert_eq!(
                schedule.captured(wid(1)),
                Some(Owed {
                    focus: Vec::new(),
                    settles: true
                })
            );
            let outcome = schedule.captured_elsewhere(wid(1), capture, now);
            let delivered: Vec<_> =
                outcome.deliveries.iter().map(|d| (d.why, d.windows.clone())).collect();
            assert_eq!(
                delivered,
                if settled {
                    vec![(Why::Landed, vec![(wid(1), cut)])]
                } else {
                    Vec::new()
                }
            );
        }
        assert!(captures(&mut schedule, now).is_empty(), "settled");
    }

    /// A repainting window cut by the display's edge with a warm capture of it waiting through the
    /// hold: that one job is raised to a settling `Priority::Repaint` capture, which starts through
    /// the hold. One already out when the watch hands the window over owes the watch nothing, and a
    /// settling capture of its own follows it.
    #[test]
    fn a_cut_repainting_end_raises_a_waiting_capture_of_it_and_follows_one_out() {
        use crate::pictures::domain::watch::{Capture, Seen, Settle};
        let now = Instant::now();
        let cut = rect(-285.0, 32.0, 1148.0, 1081.0);
        let handed_over = |schedule: &mut Schedule<(), (), ()>| {
            schedule.hold(Hold::Transition, now);
            let landing = Watch::Landing {
                tag: tag(),
                windows: vec![(target(1, 1148.0), cut)],
                behind: Vec::new(),
                slack: 20.0,
                settle: Settle::Repainted,
                until: now + Duration::from_millis(300),
            };
            schedule.watch(WatchId(1), landing, now);
            assert!(matches!(schedule.next(now, nothing_held), Next::Sky(_)));
            schedule.seen(now, &[Seen {
                window: wid(1),
                frame: Some(cut),
                capture: Capture::Missed,
            }]);
        };
        let ready = || {
            let mut schedule = Schedule::<(), (), ()>::default();
            schedule.set_display(built_in(), false, now);
            let _ = schedule.next(now, nothing_held);
            let _ = schedule.desktop_rendered(built_in(), Some(now));
            schedule
        };
        let settling = Some(Owed {
            focus: Vec::new(),
            settles: true,
        });

        let mut waiting = ready();
        waiting.hold(Hold::Transition, now);
        waiting.ask(ask(Priority::Warm, &[1]), settled_by(now));
        assert!(
            captures(&mut waiting, now).is_empty(),
            "a warm waits through the hold"
        );
        handed_over(&mut waiting);
        assert_eq!(waiting.owed().collect::<Vec<_>>(), [wid(1)], "one job");
        assert_eq!(
            captures(&mut waiting, now),
            vec![wid(1)],
            "raised, through the hold"
        );
        assert_eq!(waiting.captured(wid(1)), settling);
        assert!(waiting.owed().next().is_none());

        let mut out = ready();
        out.ask(ask(Priority::Warm, &[1]), settled_by(now));
        assert_eq!(captures(&mut out, now), vec![wid(1)], "out before the hold");
        handed_over(&mut out);
        assert_eq!(
            out.captured(wid(1)),
            Some(Owed {
                focus: Vec::new(),
                settles: false
            }),
            "owes the watch nothing"
        );
        assert_eq!(captures(&mut out, now), vec![wid(1)], "its own follows");
        assert_eq!(out.captured(wid(1)), settling);
    }

    /// A capture of the window already out began before its focus: the clipped end's capture follows
    /// it, a job of its own.
    #[test]
    fn a_clipped_focus_end_is_captured_after_a_capture_already_out() {
        let now = Instant::now();
        let mut schedule = Schedule::<(), (), ()>::default();
        schedule.ask(ask(Priority::Focus, &[1]), now);
        assert_eq!(captures(&mut schedule, now), vec![wid(1)]);
        schedule.ask_clipped(tag(), target(1, 859.0), end(), now);
        assert_eq!(
            schedule.captured(wid(1)).map(|owed| owed.focus),
            Some(Vec::new()),
            "the one out owes no one"
        );
        assert_eq!(captures(&mut schedule, now), vec![wid(1)]);
        assert_eq!(
            schedule.captured(wid(1)).map(|owed| owed.focus),
            Some(vec![(tag(), end())])
        );
        schedule.ask_clipped(tag(), target(1, 859.0), end(), now);
        schedule.ask(ask(Priority::Focus, &[1]), now);
        schedule.forget(wid(1));
        assert!(schedule.owed().next().is_none(), "forgotten, every job of it");
    }

    fn refreshed(next: Next<(), ()>) -> Vec<WindowId> {
        match next {
            Next::Refresh(set) => set.looks.iter().map(|look| look.target.window).collect(),
            other => panic!("expected a refresh round, got {other:?}"),
        }
    }

    fn seen(
        window: u32,
        frame: ScreenRect,
        capture: crate::pictures::domain::watch::Capture,
    ) -> Seen {
        Seen {
            window: wid(window),
            frame: Some(frame),
            capture,
        }
    }

    fn band(print: u8) -> crate::pictures::domain::watch::Capture {
        crate::pictures::domain::watch::Capture::Taken(crate::pictures::domain::watch::Prints {
            band: vec![print],
            thumb: None,
        })
    }

    /// A click's refresh usually reaches the service just before the hold of the flight the click
    /// starts: it waits long enough for that hold to arrive, which cancels it, as a hold cancels one
    /// asked while it holds; the flight's own rounds over the window, a frame-zero miss among them,
    /// send it nowhere else. The release asks again for each from the release plus the settle, but for
    /// one the cache holds a picture of taken `REPAINT_SETTLED` after its face turned, and its face
    /// goes before any ScreenCaptureKit call.
    #[test]
    fn a_hold_cancels_a_refresh_and_the_release_asks_again_ahead_of_screencapturekit() {
        let now = Instant::now();
        let mut schedule = Schedule::<(), (), ()>::default();
        schedule.ask(ask(Priority::Background, &[2]), settled_by(now));
        schedule.refresh(vec![target(1, 859.0)], now);
        assert_eq!(
            listed_windows(schedule.next(now, nothing_held)),
            vec![wid(2)],
            "the refresh settles"
        );
        assert!(
            matches!(schedule.next(now, nothing_held), Next::WaitUntil(at) if at == now + SETTLE_BEFORE_STARTING)
        );

        let pressed = now + Duration::from_millis(10);
        schedule.hold(Hold::Transition, pressed);
        schedule.refresh(vec![target(3, 859.0)], pressed);
        schedule.ask_on_screen(OnScreen {
            tag: tag(),
            moment: crate::pictures::domain::watch::Moment::FrameZero,
            strip: vec![(target(1, 859.0), end())],
            background: Vec::new(),
            behind: Vec::new(),
        });
        let _ = schedule.next(pressed, nothing_held);
        let _ = schedule.seen(pressed, &[seen(
            1,
            end(),
            crate::pictures::domain::watch::Capture::Missed,
        )]);
        schedule.hold(Hold::Settling, pressed);
        let settled = now + SETTLE_BEFORE_STARTING;
        assert!(
            matches!(schedule.next(settled, nothing_held), Next::Idle),
            "cancelled, and asked while held"
        );
        assert_eq!(
            schedule.owed().collect::<Vec<_>>(),
            vec![wid(2), wid(1), wid(3)],
            "still owed"
        );

        let release = now + Duration::from_millis(400);
        schedule.hold(Hold::Released, release);
        schedule.ask(ask(Priority::Background, &[4]), release);
        let pictured = |window: WindowId| {
            (window == wid(3)).then_some(Held {
                coverage: Coverage {
                    covered: ScreenSize::new(859.0, 1081.0),
                    window: ScreenSize::new(859.0, 1081.0),
                },
                taken: pressed + crate::pictures::domain::refresh::REPAINT_SETTLED,
            })
        };
        let due = release + SETTLE_BEFORE_STARTING;
        assert!(matches!(schedule.next(release, pictured), Next::WaitUntil(at) if at == due));
        assert_eq!(
            schedule.owed().collect::<Vec<_>>(),
            vec![wid(2), wid(4), wid(1)],
            "the flight pictured 3"
        );
        assert_eq!(refreshed(schedule.next(due, pictured)), vec![wid(1)]);
        assert!(
            schedule
                .refreshed(due, &[seen(1, end(), band(1))], vec![(wid(1), ())])
                .is_empty()
        );
        assert_eq!(listed_windows(schedule.next(due, pictured)), vec![wid(4)]);
    }

    /// Both windows of a refresh are looked at in one round. One the window server does not have
    /// wholly on the display is owed one ScreenCaptureKit capture at background priority, whatever is
    /// held, since its turned face keeps its size; until done, a refreshed window is owed, its capture
    /// held rather than put, and a forgotten one is not.
    #[test]
    fn a_refreshed_window_off_the_display_is_owed_one_capture_at_background_priority() {
        use crate::pictures::domain::watch::Capture;
        let now = Instant::now();
        let fits = Held {
            coverage: Coverage {
                covered: ScreenSize::new(859.0, 1081.0),
                window: ScreenSize::new(859.0, 1081.0),
            },
            taken: now,
        };
        let mut schedule = Schedule::<(), (), ()>::default();
        schedule.set_display(built_in(), false, now);
        let _ = schedule.next(now, nothing_held);
        let _ = schedule.desktop_rendered(built_in(), Some(now));
        schedule.refresh(vec![target(1, 859.0), target(2, 859.0)], settled_by(now));
        assert_eq!(schedule.owed().collect::<Vec<_>>(), vec![wid(1), wid(2)]);
        assert_eq!(
            refreshed(schedule.next(now, nothing_held)),
            vec![wid(1), wid(2)],
            "one round"
        );
        let parked = rect(1727.0, 32.0, 859.0, 1081.0);
        let seen = [seen(1, parked, Capture::Missed), seen(2, end(), band(1))];
        assert!(
            schedule.refreshed(now, &seen, vec![(wid(2), ())]).is_empty(),
            "a first capture is held"
        );
        assert_eq!(
            schedule.owed().collect::<Vec<_>>(),
            vec![wid(1), wid(2)],
            "window 2 is looked at again"
        );
        schedule.forget(wid(2));
        assert_eq!(
            schedule.owed().collect::<Vec<_>>(),
            vec![wid(1)],
            "owed ScreenCaptureKit's capture"
        );
        let settled = now + SETTLE_BEFORE_STARTING;
        assert_eq!(listed_windows(schedule.next(settled, |_| Some(fits))), vec![
            wid(1)
        ]);
    }

    /// The one record of whether there is a bar to picture, which a transition is dressed by too.
    #[test]
    fn no_bar_is_pictured_while_rini_draws_its_own() {
        let now = Instant::now();
        let mut schedule = Schedule::<(), (), ()>::default();
        assert!(
            !schedule.picture_bar(),
            "a transition before the first display is dressed with none"
        );
        schedule.set_display(built_in(), false, now);
        assert!(!schedule.picture_bar());
        let _ = schedule.next(now, nothing_held);
        schedule.hold(Hold::Settling, now);
        assert!(matches!(
            schedule.next(now + BAR_REFRESH_DELAY, nothing_held),
            Next::Idle
        ));
        schedule.set_display(built_in(), true, now);
        assert!(schedule.picture_bar(), "another app's bar, under the overlay");
    }

    fn check() -> HandoverCheck {
        HandoverCheck {
            tag: tag(),
            overlay: WindowServerId::new(77),
            overlay_origin: ScreenPoint::new(0.0, 32.0),
            display: built_in().frame,
            scale: 2.0,
            windows: vec![(wid(1), end(), 0.0)],
        }
    }

    /// A measured lift holds its hide for the overlay's own picture, so that is the next call, ahead of
    /// a landing to look at and a focus capture to list; they follow while it is taken.
    #[test]
    fn a_handover_check_takes_the_overlay_ahead_of_queued_work() {
        let now = Instant::now();
        let mut schedule = Schedule::<(), (), ()>::default();
        schedule.set_display(built_in(), false, now);
        schedule.hold(Hold::Settling, now);
        schedule.ask(ask(Priority::Focus, &[1]), now);
        schedule.watch(
            WatchId(1),
            landing(2, end(), now + Duration::from_millis(150)),
            now,
        );
        schedule.check_handover(check());
        assert!(
            matches!(schedule.next(now, nothing_held), Next::Overlay(id) if id == WindowServerId::new(77))
        );
        assert!(
            matches!(schedule.next(now, nothing_held), Next::Sky(_)),
            "the overlay is being taken"
        );
        let _ = schedule.seen(now, &[]);
        assert_eq!(schedule.overlay_taken(now, Some(())), Some(tag()));
        assert_eq!(listed_windows(schedule.next(now, nothing_held)), vec![wid(1)]);
    }

    /// The screen is taken `AFTER_LIFT` after the overlay's picture, as the research waited after the
    /// hide, once and with that picture, and never when the overlay's picture was not taken.
    #[test]
    fn the_screen_is_not_due_before_after_lift() {
        let now = Instant::now();
        let mut schedule = Schedule::<(), u8, ()>::default();
        schedule.check_handover(check());
        assert!(matches!(schedule.next(now, nothing_held), Next::Overlay(_)));
        let taken = now + Duration::from_millis(30);
        assert_eq!(schedule.overlay_taken(taken, Some(7)), Some(tag()));
        let early = taken + AFTER_LIFT - Duration::from_millis(1);
        assert!(
            matches!(schedule.next(early, nothing_held), Next::WaitUntil(at) if at == taken + AFTER_LIFT)
        );
        assert!(
            matches!(schedule.next(taken + AFTER_LIFT, nothing_held), Next::Screen(due, 7) if due == check())
        );
        assert!(
            matches!(schedule.next(taken + AFTER_LIFT, nothing_held), Next::Idle),
            "once"
        );

        schedule.check_handover(check());
        let _ = schedule.next(now, nothing_held);
        assert_eq!(
            schedule.overlay_taken(now, None),
            Some(tag()),
            "its client is told all the same"
        );
        assert!(
            matches!(schedule.next(now + AFTER_LIFT, nothing_held), Next::Idle),
            "nothing to compare"
        );
        assert_eq!(schedule.overlay_taken(now, Some(7)), None, "no check under way");
    }

    /// A transition beginning drops the check under way, before its overlay's picture or after, and the
    /// overlay's picture with it: the screen would show the new overlay.
    #[test]
    fn a_transition_beginning_drops_the_handover_check() {
        let now = Instant::now();
        let mut schedule = Schedule::<(), Rc<()>, ()>::default();
        schedule.check_handover(check());
        schedule.hold(Hold::Transition, now);
        assert!(matches!(schedule.next(now, nothing_held), Next::Idle), "before");
        schedule.hold(Hold::Settling, now);
        schedule.check_handover(check());
        let _ = schedule.next(now, nothing_held);
        let picture = Rc::new(());
        schedule.overlay_taken(now, Some(picture.clone()));
        schedule.hold(Hold::Settling, now);
        assert_eq!(Rc::strong_count(&picture), 2, "settling: the lift it measures");
        schedule.hold(Hold::Transition, now);
        assert_eq!(Rc::strong_count(&picture), 1, "the overlay's picture is let go");
        assert!(
            matches!(schedule.next(now + AFTER_LIFT, nothing_held), Next::Idle),
            "after"
        );
    }
}
