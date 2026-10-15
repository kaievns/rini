//! The pictures service: the thread that takes every background picture of a window, the desktop and
//! the bar, the cache those pictures land in, and the handle every client holds.
//!
//! One thread, `pictures`, runs the schedule (`crate::pictures::domain::schedule`) and makes the
//! service's own calls: its SkyLight captures, one at a time, a handover check's two pictures, which a
//! thread of the check's own compares, and its ScreenCaptureKit asks, whose completions post back to
//! it. A transition's dressing is composited on
//! the caller's thread until its display's first render lands, and the flight engine takes its spawn
//! capture of a window that has just opened on the `transitions` thread as it composes (Known debt
//! in `src/pictures/docs/README.md`).
//! A client asks, watches, hints, holds and forgets through a channel, reads from any thread through
//! one lock, and hears of what lands through the sink it was started with. See "The service" in
//! `src/pictures/docs/README.md`.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use objc2_core_foundation::{CFRetained, CGRect};
use objc2_core_graphics::CGImage;
use rini_core::ids::{WindowId, WindowServerId};
use rini_geometry::space::{FromCg, ScreenRect, ScreenSize, ToCg};
use rini_runloop::channel::{self, Receiver, Sender};
use tracing::{debug, warn};

use crate::pictures::domain::cache::{Delivered, PictureCache};
use crate::pictures::domain::coverage::spans_display;
use crate::pictures::domain::handover::{HandoverCheck, HandoverDiff, compare};
use crate::pictures::domain::knowledge::{Knowledge, Learned};
use crate::pictures::domain::print::bodies_differ;
use crate::pictures::domain::region::apart;
use crate::pictures::domain::schedule::{Ask, Display, Held, Hold, Next, Owed, Schedule};
use crate::pictures::domain::target::PictureTarget;
use crate::pictures::domain::watch::{
    Capture, CapturedElsewhere, Hint, Look, OnScreen, Outcome, Prints, Seen, Set, Tag, Watch, WatchId, Why,
};
use crate::pictures::event::Event;
use crate::pictures::platform::desktop::{BarStrip, DesktopBackdrop, bar_strip, desktop_backdrop_windows};
use crate::pictures::platform::measure::{band_print, body_samples, picture_thumbprint, rgba};
use crate::pictures::platform::picture::{Picture, Pixels, stand_in_picture, stand_in_image};
use crate::pictures::platform::screencapturekit::{self, Prepared};
use crate::pictures::platform::skylight::{self, capture_list_via_skylight};
use crate::windows::platform::window_server::{key_focus_pid, server_frame};

/// What a client asks of the service.
#[derive(Debug)]
pub enum Command {
    Ask(Ask),
    AskOnScreen(OnScreen),
    Refresh(Vec<PictureTarget>),
    Watch(WatchId, Watch),
    Retarget(WatchId, Watch),
    Stop(WatchId),
    Hint(Hint),
    /// The picture last delivered of the window under the tag shows it focused.
    KeepFocused(Tag, WindowId),
    Hold(Hold),
    Forget(WindowId),
    SetDisplay { display: Display, picture_bar: bool },
    CheckHandover(HandoverCheck),
}

/// What a call the service made brought back, posted from ScreenCaptureKit's queues.
enum Done {
    Listed {
        target: PictureTarget,
        prepared: Option<Prepared>,
    },
    Captured {
        window: WindowId,
        picture: Option<Picture>,
    },
    Desktop {
        display: Display,
        picture: Option<Picture>,
    },
    /// A handover check's two pictures compared, off the thread.
    Compared {
        tag: Tag,
        windows: Vec<(WindowId, HandoverDiff)>,
    },
}

/// Where a client hears of what lands. Called on the service's thread.
pub type Sink = Box<dyn Fn(Event) + Send>;

/// What a transition on one display is dressed with: the desktop behind its tiles and the bar over
/// them.
pub struct Dressing {
    pub desktop: Option<Picture>,
    pub bar: Option<Picture>,
    /// Where the bar sits. With no picture, the picture already drawn stays.
    pub strip: Option<ScreenRect>,
}

/// The handle to the service. Any thread may hold one.
#[derive(Clone)]
pub struct Pictures {
    commands: Sender<Command>,
    shared: Arc<Mutex<Shared>>,
    /// Bumped by every `hold(Hold::Transition)` before it is sent: the overlay can go up while the
    /// service is reading the bar off the framebuffer, before the hold has reached it.
    transitions: Arc<AtomicU64>,
    /// The last watch id handed out.
    watches: Arc<AtomicU64>,
}

#[derive(Default)]
struct Shared {
    cache: PictureCache<Picture>,
    schedule: Schedule<Prepared, Whole, Picture>,
    /// The pictures of the schedule's display, which belong to it alone.
    display: DisplayPictures,
    learned: Learned,
    delivered: Delivered<Picture>,
    /// The stand-in's pixel, drawn once.
    stand_in: Option<CFRetained<CGImage>>,
}

/// Every picture of `window` and all that is known of it, gone.
fn forget_window(
    shared: &mut Shared,
    looks_checked: &mut HashMap<WindowId, (Instant, Instant)>,
    window: WindowId,
) {
    forget_pictures(shared, looks_checked, window);
    shared.learned.forget(WindowServerId::from(window));
}

/// Every picture of `window`, held, owed or delivered, and its focused look's last check, gone.
/// What is known of it stays: a window pictured twice shares its server id with the managed one.
fn forget_pictures(
    shared: &mut Shared,
    looks_checked: &mut HashMap<WindowId, (Instant, Instant)>,
    window: WindowId,
) {
    shared.cache.forget(window);
    shared.schedule.forget(window);
    shared.delivered.forget(window);
    looks_checked.remove(&window);
}

/// The pictures of the display the overlay flies on. Which display that is, the schedule holds.
#[derive(Default)]
struct DisplayPictures {
    /// The desktop as ScreenCaptureKit rendered it: the only source that reliably has the wallpaper.
    desktop: Option<Picture>,
    /// The last usable picture of the bar, and the strip it covers.
    bar: Option<(Picture, ScreenRect)>,
    /// The desktop's windows, listed after a baked call's: read once per display, and again when a
    /// baked composite comes back another shape, which a desktop window gone from it makes.
    desktop_windows: Option<DesktopBackdrop>,
}

impl DisplayPictures {
    /// What is held for a transition on `display`: nothing for any display but the one held, and no
    /// bar once none is on screen.
    fn for_display(
        &self,
        held: Option<Display>,
        display: Display,
        bar_on_screen: bool,
    ) -> (Option<Picture>, Option<(Picture, ScreenRect)>) {
        if held != Some(display) {
            return (None, None);
        }
        (self.desktop.clone(), self.bar.clone().filter(|_| bar_on_screen))
    }
}

/// The overlay now flies on `display`. A change leaves nothing of the display left behind; a bar
/// no longer pictured is dropped.
fn set_display<P, O, C>(
    schedule: &mut Schedule<P, O, C>,
    held: &mut DisplayPictures,
    display: Display,
    picture_bar: bool,
    now: Instant,
) {
    if schedule.set_display(display, picture_bar, now) {
        *held = DisplayPictures::default();
    } else if !picture_bar {
        held.bar = None;
    }
}

/// A render of `display`'s desktop came back. Held only when it spans the display it was asked of
/// and that display is still the one held: a render of the external drawn behind a flight on the
/// built-in display shows its top-left corner, zoomed.
fn land_desktop<P, O, C>(
    schedule: &mut Schedule<P, O, C>,
    held: &mut DisplayPictures,
    display: Display,
    picture: Option<Picture>,
) -> bool {
    let picture =
        picture.filter(|picture| spans_display(picture.coverage.covered, display.frame.size));
    let kept = schedule.desktop_rendered(display, picture.as_ref().map(|picture| picture.taken));
    if kept {
        held.desktop = picture;
    }
    kept
}

/// Puts a capture in the cache; true when its pixels are now the ones held, and then every sink hears
/// `Taken`.
fn put(cache: &mut PictureCache<Picture>, window: WindowId, picture: Picture, events: &mut Vec<Event>) -> bool {
    let took = cache.insert(window, picture.clone());
    if took {
        events.push(Event::Taken { window, picture });
    }
    took
}

/// What a ScreenCaptureKit capture that came back tells the clients, once the cache has had it
/// (`put`): when it took it, a delivery to each client owed it as a focus change, sent `now`; and,
/// owed to a repaint watch, what the watch makes of it by its thumbprint
/// (`Schedule::captured_elsewhere`), a capture the cache refused or an empty one judged as such.
/// Nothing for a window forgotten meanwhile (`owed` is `None`).
fn landing<P, O, C>(
    cache: &mut PictureCache<Picture>,
    delivered: &mut Delivered<Picture>,
    schedule: &mut Schedule<P, O, C>,
    now: Instant,
    window: WindowId,
    owed: Option<Owed>,
    picture: Option<Picture>,
) -> Vec<Event> {
    let mut events = Vec::new();
    let Some(owed) = owed else {
        return events;
    };
    let captured = picture.is_some();
    let took = picture.filter(|picture| put(cache, window, picture.clone(), &mut events));
    if let Some(picture) = &took {
        for (tag, frame) in owed.focus {
            delivered.record(tag, window, picture.clone());
            events.push(Event::Delivered {
                tag,
                why: Why::FocusChanged,
                sent: now,
                pictures: vec![(window, picture.clone(), frame)],
            });
        }
    }
    if owed.settles {
        let capture = match &took {
            Some(picture) => {
                picture_thumbprint(picture).map_or(CapturedElsewhere::Missed, CapturedElsewhere::Taken)
            }
            None if captured => CapturedElsewhere::Refused,
            None => CapturedElsewhere::Missed,
        };
        let outcome = schedule.captured_elsewhere(window, capture, now);
        let took = took.into_iter().map(|picture| (window, picture)).collect();
        events.extend(told(delivered, now, outcome, &took));
    }
    events
}

/// What `outcome` tells the clients: every delivery as the watches decided it, of the pictures the
/// cache `took`, stamped `now` and recorded as delivered; and every window lost.
fn told(
    delivered: &mut Delivered<Picture>,
    now: Instant,
    outcome: Outcome,
    took: &HashMap<WindowId, Picture>,
) -> Vec<Event> {
    let mut events = Vec::new();
    for delivery in outcome.deliveries {
        let pictures: Vec<(WindowId, Picture, ScreenRect)> = delivery
            .windows
            .iter()
            .filter_map(|(window, frame)| Some((*window, took.get(window)?.clone(), *frame)))
            .collect();
        for (window, picture, _) in &pictures {
            delivered.record(delivery.tag, *window, picture.clone());
        }
        events.push(Event::Delivered {
            tag: delivery.tag,
            why: delivery.why,
            sent: now,
            pictures,
        });
    }
    events.extend(outcome.lost.into_iter().map(|(tag, window)| Event::Lost { tag, window }));
    events
}

/// The windows of `looks` to capture, at their server frames: those one of their accepts takes there
/// on the display held. None with no display held.
fn wanted(looks: &[Look], frames: &[Option<CGRect>], display: Option<Display>) -> Vec<skylight::OnScreen> {
    let Some(display) = display else {
        return Vec::new();
    };
    looks
        .iter()
        .zip(frames)
        .filter_map(|(look, frame)| {
            let frame = (*frame)?;
            let at = ScreenRect::from_cg(frame);
            look.accepts.iter().any(|accept| accept.accepts(at, display.frame)).then_some(
                skylight::OnScreen {
                    window: look.target.window,
                    server_id: look.target.server_id,
                    frame,
                },
            )
        })
        .collect()
}

/// Each picture captured, with its prints: its band print, and its thumbprint only where a look at
/// its window asks for one.
fn with_prints(looks: &[Look], captured: Vec<(WindowId, Picture)>) -> Vec<(WindowId, Picture, Option<Prints>)> {
    captured
        .into_iter()
        .map(|(window, picture)| {
            let thumbprint = looks.iter().any(|look| look.target.window == window && look.thumbprint);
            let prints = band_print(&picture).map(|band| Prints {
                band,
                thumb: thumbprint.then(|| picture_thumbprint(&picture)).flatten(),
            });
            (window, picture, prints)
        })
        .collect()
}

/// What each window looked at was seen as: at its server frame, and captured as `captures` says.
fn seen_as(looked: Vec<(WindowId, Option<CGRect>)>, captures: &HashMap<WindowId, Capture>) -> Vec<Seen> {
    looked
        .into_iter()
        .map(|(window, frame)| Seen {
            window,
            frame: frame.map(ScreenRect::from_cg),
            capture: captures.get(&window).cloned().unwrap_or(Capture::Missed),
        })
        .collect()
}

/// What a watch's round of SkyLight captures tells the clients. Each picture is put to the cache
/// first, and the schedule is told what each window looked at was seen as: only a picture the cache
/// took is a capture it may deliver or judge news by. Every delivery it decides is then sent as it
/// decided it, stamped `now`, and recorded as delivered.
fn round<P, O, C>(
    cache: &mut PictureCache<Picture>,
    delivered: &mut Delivered<Picture>,
    schedule: &mut Schedule<P, O, C>,
    now: Instant,
    looked: Vec<(WindowId, Option<CGRect>)>,
    captured: Vec<(WindowId, Picture, Option<Prints>)>,
) -> Vec<Event> {
    let mut events = Vec::new();
    let mut took = HashMap::new();
    let mut captures = HashMap::new();
    for (window, picture, prints) in captured {
        let capture = if put(cache, window, picture.clone(), &mut events) {
            took.insert(window, picture);
            prints.map_or(Capture::Missed, Capture::Taken)
        } else {
            Capture::Refused
        };
        captures.insert(window, capture);
    }
    let outcome = schedule.seen(now, &seen_as(looked, &captures));
    events.extend(told(delivered, now, outcome, &took));
    events
}

/// What a refresh's round of SkyLight captures tells the clients. Nothing is put while a window's
/// refresh polls: each capture is judged by whether the cache would take it, and the schedule holds
/// the latest. The capture each window settled on is then put, and every sink hears of it once.
fn refreshed<P, O>(
    cache: &mut PictureCache<Picture>,
    schedule: &mut Schedule<P, O, Picture>,
    now: Instant,
    looked: Vec<(WindowId, Option<CGRect>)>,
    captured: Vec<(WindowId, Picture, Option<Prints>)>,
) -> Vec<Event> {
    let mut held = Vec::new();
    let mut captures = HashMap::new();
    for (window, picture, prints) in captured {
        let capture = match prints {
            _ if !cache.takes(window, &picture) => Capture::Refused,
            Some(prints) => {
                held.push((window, picture));
                Capture::Taken(prints)
            }
            None => Capture::Missed,
        };
        captures.insert(window, capture);
    }
    let mut events = Vec::new();
    for (window, picture) in schedule.refreshed(now, &seen_as(looked, &captures), held) {
        put(cache, window, picture, &mut events);
    }
    events
}

/// Whether a transition was asked that the thread has not applied yet: the overlay may be up, and the
/// press behind it waits on whatever the thread does before it applies the hold and the frame zero
/// asked after it.
fn transition_asked(asked: u64, applied: u64) -> bool { asked != applied }

/// Whether a bar read is free of the overlay: no transition was asked before it that the thread had
/// not yet applied, and none was asked while it ran.
fn bar_read_kept(asked_before: u64, applied: u64, asked_after: u64) -> bool {
    !transition_asked(asked_before, applied) && asked_after == asked_before
}

/// A picture of a handover check, the overlay's own or the screen's, undrawn until it is compared.
struct Whole(CFRetained<CGImage>);

impl Pictures {
    /// Starts the service on its own thread. Every sink hears of every picture the cache takes.
    pub fn start(sinks: Vec<Sink>) -> Self {
        let (commands, requests) = channel::channel();
        let (done_tx, done) = channel::channel();
        let pictures = Self {
            commands,
            shared: Arc::default(),
            transitions: Arc::default(),
            watches: Arc::default(),
        };
        let service = Service {
            requests,
            done,
            done_tx,
            shared: pictures.shared.clone(),
            transitions: pictures.transitions.clone(),
            applied_transitions: 0,
            looks_checked: HashMap::new(),
            sinks,
        };
        std::thread::Builder::new()
            .name("pictures".to_string())
            .spawn(move || rini_runloop::executor::Executor::run(service.run()))
            .expect("the pictures thread starts");
        pictures
    }

    /// A handle with no service behind it, whose commands the caller reads.
    #[cfg(test)]
    pub fn detached() -> (Self, Receiver<Command>) {
        let (commands, requests) = channel::channel();
        let pictures = Self {
            commands,
            shared: Arc::default(),
            transitions: Arc::default(),
            watches: Arc::default(),
        };
        (pictures, requests)
    }

    pub fn ask(&self, ask: Ask) { self.commands.send(Command::Ask(ask)) }

    /// Windows on screen, captured now through SkyLight and delivered under the ask's tag.
    pub fn ask_on_screen(&self, ask: OnScreen) { self.commands.send(Command::AskOnScreen(ask)) }

    /// Fresh pictures of windows whose look has just turned, whatever the cache holds of them, once
    /// nothing holds the service: through SkyLight, blur and all, where the window server has a window
    /// wholly on the display, every window due in one round until each one's look has settled, and
    /// put once; through ScreenCaptureKit at background priority anywhere else.
    pub fn refresh(&self, targets: Vec<PictureTarget>) { self.commands.send(Command::Refresh(targets)) }

    /// Starts `watch`, which runs until stopped.
    pub fn watch(&self, watch: Watch) -> WatchId {
        let id = WatchId(self.watches.fetch_add(1, Ordering::AcqRel) + 1);
        self.commands.send(Command::Watch(id, watch));
        id
    }

    pub fn retarget(&self, id: WatchId, watch: Watch) { self.commands.send(Command::Retarget(id, watch)) }

    pub fn stop(&self, id: WatchId) { self.commands.send(Command::Stop(id)) }

    pub fn hint(&self, hint: Hint) { self.commands.send(Command::Hint(hint)) }

    /// The picture `tag` was last delivered of `window` shows it focused: kept as its focused look.
    pub fn keep_focused(&self, tag: Tag, window: WindowId) {
        self.commands.send(Command::KeepFocused(tag, window))
    }

    pub fn hold(&self, hold: Hold) {
        if hold == Hold::Transition {
            self.transitions.fetch_add(1, Ordering::AcqRel);
        }
        self.commands.send(Command::Hold(hold))
    }

    pub fn forget(&self, window: WindowId) { self.commands.send(Command::Forget(window)) }

    pub fn set_display(&self, display: Display, picture_bar: bool) {
        self.commands.send(Command::SetDisplay { display, picture_bar })
    }

    /// Measures a lift: the overlay alone as the next SkyLight call, its client told `OverlayTaken`,
    /// then the screen `AFTER_LIFT` later, and each window compared, told as `Handover`.
    pub fn check_handover(&self, check: HandoverCheck) { self.commands.send(Command::CheckHandover(check)) }

    /// The picture held of `window` if it is worth drawing, however old it is.
    pub fn usable(&self, window: WindowId) -> Option<Picture> {
        self.lock().cache.usable(window).cloned()
    }

    /// The pictures worth drawing of `windows`, in the order asked, leaving out any with none.
    pub fn usable_many(&self, windows: &[WindowId]) -> Vec<(WindowId, Picture)> {
        let shared = self.lock();
        windows
            .iter()
            .filter_map(|window| Some((*window, shared.cache.usable(*window)?.clone())))
            .collect()
    }

    /// The last picture of `window` delivered as showing it focused, until a newer capture shows
    /// different content.
    pub fn focused(&self, window: WindowId) -> Option<Picture> { self.lock().cache.focused(window).cloned() }

    /// What is known of the window: its translucency and material.
    pub fn knowledge(&self, window: WindowServerId) -> Knowledge { self.lock().learned.get(window) }

    /// A stand-in for a window of `size`, never cached. `None` only if its pixel cannot be drawn.
    pub fn stand_in(&self, size: ScreenSize) -> Option<Picture> {
        let mut shared = self.lock();
        if shared.stand_in.is_none() {
            shared.stand_in = stand_in_image();
        }
        Some(stand_in_picture(size, shared.stand_in.clone()?))
    }

    /// `display` held as the overlay's, with `desktop` as its desktop, as a render landed for it.
    #[cfg(test)]
    pub fn hold_desktop(&self, display: Display, desktop: Picture) {
        let mut shared = self.lock();
        let Shared { schedule, display: held, .. } = &mut *shared;
        set_display(schedule, held, display, false, Instant::now());
        held.desktop = Some(desktop);
    }

    /// `look` held as `window`'s focused look, as a delivery kept focused would hold it.
    #[cfg(test)]
    pub fn hold_focused(&self, window: WindowId, look: Picture) { self.lock().cache.keep_focused(window, look) }

    /// Hands the cache a picture taken outside the service. True when its pixels are now the ones held.
    pub fn insert(&self, window: WindowId, picture: Picture) -> bool {
        self.lock().cache.insert(window, picture)
    }

    /// The desktop and the bar to dress a transition on `display` with: what the service holds of that
    /// display, and, until its first render lands, a desktop composited now on the caller's thread,
    /// without which it draws black. The bar is read for presence on every transition, so a bar that
    /// has gone is not drawn, and composited once per display.
    /// Whether there is a bar to picture is the service's, as the display it was last told says.
    pub fn dressing(&self, display: Display) -> Dressing {
        let picture_bar = self.lock().schedule.picture_bar();
        let strip = picture_bar.then(|| bar_strip(display.frame));
        let bar_on_screen = strip.as_ref().is_some_and(|strip| strip.bounds.is_some());
        let (desktop, bar) = {
            let shared = self.lock();
            shared.display.for_display(shared.schedule.display(), display, bar_on_screen)
        };
        let desktop = desktop.or_else(|| composite_desktop(display));
        let Some(strip) = strip else {
            return Dressing {
                desktop,
                bar: None,
                strip: None,
            };
        };
        let (bar, strip) = match bar {
            Some((picture, strip)) => (Some(picture), Some(strip)),
            None => {
                let (picture, strip) = bar_of(&strip, display.scale);
                if let (Some(picture), Some(strip)) = (&picture, strip) {
                    self.keep_bar(display, picture.clone(), strip);
                }
                (picture, strip)
            }
        };
        Dressing { desktop, bar, strip }
    }

    fn keep_bar(&self, display: Display, picture: Picture, strip: ScreenRect) {
        let mut shared = self.lock();
        if shared.schedule.display() == Some(display) {
            shared.display.bar = Some((picture, strip));
        }
    }

    fn lock(&self) -> MutexGuard<'_, Shared> { self.shared.lock().unwrap() }
}

/// What the cache holds of `window` that is worth drawing, as the warm filter reads it.
fn held(cache: &PictureCache<Picture>, window: WindowId) -> Option<Held> {
    cache.usable(window).map(|picture| Held {
        coverage: picture.coverage,
        taken: picture.taken,
    })
}

/// The desktop of `display` composited from its windows, for a transition before any render of it has
/// landed, with or without its wallpaper: the overlay keeps no desktop between flights to prefer. See
/// "Which route serves, and why the render wins" in `src/pictures/docs/capture-research.md`.
fn composite_desktop(display: Display) -> Option<Picture> {
    let size = display.frame.size;
    let desktop = desktop_backdrop_windows(display.frame);
    let usable = capture_list_via_skylight(&desktop.windows, size, display.scale)
        .filter(|picture| spans_display(picture.coverage.covered, size));
    if usable.is_none() {
        debug!(
            windows = desktop.windows.len(),
            has_wallpaper = desktop.has_wallpaper(),
            wanted = format!("{:.0}x{:.0}", size.width, size.height),
            "no drawable desktop yet; the backdrop stays empty"
        );
    }
    usable
}

/// The bar listed in `strip`, captured on its own to keep its alpha, and the strip it covers. See "The
/// bar has to be captured on its own" in `src/pictures/docs/capture-research.md`.
fn bar_of(strip: &BarStrip, scale: f64) -> (Option<Picture>, Option<ScreenRect>) {
    let Some(bounds) = strip.bounds else {
        return (None, None);
    };
    let picture = capture_list_via_skylight(&strip.windows, bounds.size, scale)
        .filter(|picture| picture.fits(bounds.size));
    (picture, Some(bounds))
}

/// Whose SkyLight round it is: the watches' a transition waits on, or a refresh's at rest.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Looking {
    Watches,
    Refresh,
}

/// The thread's side: the schedule's calls made, and what they bring back put where it belongs.
struct Service {
    requests: Receiver<Command>,
    done: Receiver<Done>,
    done_tx: Sender<Done>,
    shared: Arc<Mutex<Shared>>,
    transitions: Arc<AtomicU64>,
    /// How many `Hold::Transition` this thread has applied, against `transitions` asked.
    applied_transitions: u64,
    /// The focused look and the newer picture last compared for each window, by when each was taken:
    /// a pair is compared once.
    looks_checked: HashMap<WindowId, (Instant, Instant)>,
    sinks: Vec<Sink>,
}

impl Service {
    async fn run(mut self) {
        loop {
            let wait = self.make_calls();
            tokio::select! {
                biased;
                command = self.requests.recv() => match command {
                    Some((span, command)) => {
                        let _guard = span.enter();
                        self.apply(command);
                        // Everything already asked is applied before the next call, so the newest wins.
                        while let Ok((span, command)) = self.requests.try_recv() {
                            let _guard = span.enter();
                            self.apply(command);
                        }
                    }
                    None => break,
                },
                Some((span, done)) = self.done.recv() => {
                    let _guard = span.enter();
                    self.finish(done);
                }
                _ = rini_runloop::executor::sleep(wait.unwrap_or_default()), if wait.is_some() => {}
            }
        }
    }

    fn apply(&mut self, command: Command) {
        let now = Instant::now();
        let mut shared = self.shared.lock().unwrap();
        match command {
            Command::Ask(ask) => shared.schedule.ask(ask, now),
            Command::AskOnScreen(ask) => shared.schedule.ask_on_screen(ask),
            Command::Refresh(targets) => shared.schedule.refresh(targets, now),
            Command::Watch(id, watch) => shared.schedule.watch(id, watch, now),
            Command::Retarget(id, watch) => shared.schedule.retarget(id, watch, now),
            Command::Stop(id) => shared.schedule.stop(id),
            Command::Hint(hint) => {
                if let Hint::Focused(window) = hint {
                    shared.learned.focused(window, now);
                }
                shared.schedule.hint(hint, now)
            }
            Command::KeepFocused(tag, window) => {
                if let Some(look) = shared.delivered.under(tag, window).cloned() {
                    shared.cache.keep_focused(window, look);
                }
            }
            Command::Hold(hold) => {
                if hold == Hold::Transition {
                    self.applied_transitions += 1;
                }
                shared.schedule.hold(hold, now);
                if hold == Hold::Released {
                    shared.delivered.clear();
                    drop(shared);
                    self.at_rest();
                }
            }
            Command::Forget(window) => forget_window(&mut shared, &mut self.looks_checked, window),
            Command::SetDisplay { display, picture_bar } => {
                let Shared { schedule, display: held, .. } = &mut *shared;
                set_display(schedule, held, display, picture_bar, now);
            }
            Command::CheckHandover(check) => shared.schedule.check_handover(check),
        }
    }

    /// What no transition holding the service allows: regions kept as copies, and focused looks
    /// compared with what was captured since.
    fn at_rest(&mut self) {
        self.keep_copies();
        self.check_focused();
    }

    /// Every region the cache holds, as a picture or a focused look, replaced by a copy of its own: a
    /// region keeps its whole composite alive. Copied off the lock, and put back only over the picture
    /// it was made of. Why a transition draws regions at all: "A region is drawn in a transition" in
    /// `src/pictures/docs/README.md`.
    fn keep_copies(&self) {
        let is_region = |picture: &&Picture| matches!(picture.image, Pixels::Region { .. });
        let (pictures, looks): (Vec<(WindowId, Picture)>, Vec<(WindowId, Picture)>) = {
            let shared = self.shared.lock().unwrap();
            let regions = |held: &mut dyn Iterator<Item = (&WindowId, &Picture)>| {
                held.filter(|(_, picture)| is_region(picture)).map(|(window, picture)| (*window, picture.clone())).collect()
            };
            (regions(&mut shared.cache.iter()), regions(&mut shared.cache.focused_looks()))
        };
        for (window, region) in pictures {
            let Some(copy) = region.kept() else { continue };
            let taken = region.taken;
            self.shared.lock().unwrap().cache.replace_if(window, |held| held.taken == taken, copy);
        }
        for (window, region) in looks {
            let Some(copy) = region.kept() else { continue };
            let taken = region.taken;
            self.shared.lock().unwrap().cache.replace_focused_if(window, |held| held.taken == taken, copy);
        }
    }

    /// Drops each focused look a newer capture of its window shows different content than, below the
    /// band where focus shows. Compared off the lock, once per pair.
    fn check_focused(&mut self) {
        let pairs: Vec<(WindowId, Picture, Picture)> = {
            let shared = self.shared.lock().unwrap();
            shared
                .cache
                .focused_looks()
                .filter_map(|(window, look)| {
                    let newer = shared.cache.get(*window).filter(|newer| newer.taken > look.taken)?;
                    Some((*window, look.clone(), newer.clone()))
                })
                .collect()
        };
        for (window, look, newer) in pairs {
            let pair = (look.taken, newer.taken);
            if self.looks_checked.insert(window, pair) == Some(pair) {
                continue;
            }
            let stale = match (body_samples(&look.image), body_samples(&newer.image)) {
                (Some(look), Some(newer)) => bodies_differ(&look, &newer),
                _ => true,
            };
            if stale {
                let taken = look.taken;
                self.shared.lock().unwrap().cache.drop_focused_if(window, |held| held.taken == taken);
            }
        }
    }

    /// Makes every call the schedule has ready until a command is queued; how long nothing is due
    /// for, if anything is. A hold that arrives during a call is in force before the next one starts.
    fn make_calls(&mut self) -> Option<Duration> {
        loop {
            if !self.requests.is_empty() {
                return Some(Duration::ZERO);
            }
            let now = Instant::now();
            let next = {
                let mut shared = self.shared.lock().unwrap();
                let Shared { cache, schedule, .. } = &mut *shared;
                schedule.next(now, |window| held(cache, window))
            };
            let done = self.done_tx.clone();
            match next {
                Next::Lost(lost) => {
                    let events = lost.into_iter().map(|(tag, window)| Event::Lost { tag, window }).collect();
                    self.tell(events);
                }
                Next::Sky(sets) => {
                    self.look(sets, Looking::Watches);
                }
                Next::Refresh(set) => {
                    if self.look(vec![set], Looking::Refresh) {
                        return Some(Duration::ZERO);
                    }
                }
                Next::List(targets) => screencapturekit::list(targets, move |target, prepared| {
                    done.send(Done::Listed { target, prepared })
                }),
                Next::Capture { target, prepared } => {
                    screencapturekit::capture(target, prepared, move |picture| {
                        done.send(Done::Captured {
                            window: target.window,
                            picture,
                        })
                    })
                }
                Next::Desktop(display) => screencapturekit::render_desktop(display, move |picture| {
                    done.send(Done::Desktop { display, picture })
                }),
                Next::Bar(display) => self.picture_bar(display),
                Next::Overlay(overlay) => self.take_overlay(overlay),
                Next::Screen(check, overlay) => self.compare_screen(check, overlay),
                Next::WaitUntil(at) => return Some(at.saturating_duration_since(now)),
                Next::Idle => return None,
            }
        }
    }

    /// Reads the server frame of every window `sets` names, captures those where they are wanted, set
    /// by set and never two overlapping windows in one call, and puts what came back where it belongs
    /// (`put_round`). A refresh's round makes no call once a transition is asked, and what it did not
    /// call is not seen; true when it stopped so.
    fn look(&mut self, sets: Vec<Set>, looking: Looking) -> bool {
        let (asked, applied) = (self.transitions.clone(), self.applied_transitions);
        let halting = looking == Looking::Refresh;
        let halt = move || halting && transition_asked(asked.load(Ordering::Acquire), applied);
        let display = self.shared.lock().unwrap().schedule.display();
        // Taken from the shared state only once something is captured: most looks are landing polls.
        let mut known: Option<(Learned, Option<DesktopBackdrop>)> = None;
        let mut key = None;
        let mut looks = Vec::new();
        let mut looked = Vec::new();
        let mut captured: Vec<(WindowId, Picture)> = Vec::new();
        let mut desktop_stale = false;
        let mut halted: Vec<WindowId> = Vec::new();
        for set in sets {
            let frames: Vec<Option<CGRect>> =
                set.looks.iter().map(|look| server_frame(look.target.server_id)).collect();
            let wanted = wanted(&set.looks, &frames, display);
            if let (Some(display), false) = (display, wanted.is_empty()) {
                let behind: Vec<(WindowServerId, CGRect)> =
                    set.behind.iter().filter_map(|id| Some((*id, server_frame(*id)?))).collect();
                let (learned, desktop) = known.get_or_insert_with(|| {
                    key = key_focus_pid();
                    let shared = self.shared.lock().unwrap();
                    (shared.learned.clone(), shared.display.desktop_windows.clone())
                });
                let desktop = desktop.get_or_insert_with(|| desktop_backdrop_windows(display.frame));
                let frames: Vec<ScreenRect> =
                    wanted.iter().map(|window| ScreenRect::from_cg(window.frame)).collect();
                for group in apart(&frames) {
                    let windows: Vec<skylight::OnScreen> = group.into_iter().map(|at| wanted[at]).collect();
                    let on = display.frame.to_cg();
                    let batch =
                        skylight::capture_on_screen(&windows, &behind, desktop, on, learned, key, &halt);
                    desktop_stale |= batch.desktop_stale;
                    captured.extend(batch.pictures);
                    halted.extend(batch.halted);
                }
            }
            let windows = set.looks.iter().map(|look| look.target.window);
            looked.extend(windows.zip(frames).filter(|(window, _)| !halted.contains(window)));
            looks.extend(set.looks);
        }
        let captured = with_prints(&looks, captured);
        self.put_round(looking, looked, captured, known, desktop_stale);
        !halted.is_empty()
    }

    /// What a round of SkyLight captures brought back, put where it belongs: what was learned of each
    /// window, and the desktop's windows unless the round found them `desktop_stale`; the pictures and
    /// what the schedule makes of them (`round` for a watch's, `refreshed` for a refresh's), told; and,
    /// released, what no hold allows, once the cache took any.
    fn put_round(
        &mut self,
        looking: Looking,
        looked: Vec<(WindowId, Option<CGRect>)>,
        captured: Vec<(WindowId, Picture, Option<Prints>)>,
        known: Option<(Learned, Option<DesktopBackdrop>)>,
        desktop_stale: bool,
    ) {
        let (events, released) = {
            let mut shared = self.shared.lock().unwrap();
            if let Some((learned, desktop)) = known {
                shared.learned = learned;
                shared.display.desktop_windows = desktop.filter(|_| !desktop_stale);
            }
            let Shared { cache, delivered, schedule, .. } = &mut *shared;
            let now = Instant::now();
            let events = match looking {
                Looking::Watches => round(cache, delivered, schedule, now, looked, captured),
                Looking::Refresh => refreshed(cache, schedule, now, looked, captured),
            };
            (events, !schedule.held())
        };
        let took_any = events.iter().any(|event| matches!(event, Event::Taken { .. }));
        self.tell(events);
        if released && took_any {
            self.at_rest();
        }
    }

    fn tell(&self, events: Vec<Event>) {
        for event in events {
            for sink in &self.sinks {
                sink(event.clone());
            }
        }
    }

    /// Whether a transition was asked that this thread has not applied yet.
    fn transition_waits(&self) -> bool {
        transition_asked(self.transitions.load(Ordering::Acquire), self.applied_transitions)
    }

    /// The overlay alone, for the handover check under way, and its client told at once that the
    /// overlay may come down. Not taken with a transition asked: that press has hidden the overlay, and
    /// its frame zero is asked of this thread.
    fn take_overlay(&mut self, overlay: WindowServerId) {
        let picture = if self.transition_waits() { None } else { skylight::capture_alone(overlay).map(Whole) };
        let taken = picture.is_some();
        let tag = self.shared.lock().unwrap().schedule.overlay_taken(Instant::now(), picture);
        if let Some(tag) = tag {
            self.tell(vec![Event::OverlayTaken { tag, taken }]);
        }
    }

    /// The screen, for `check` against the overlay's picture taken for it: drawn and compared over each
    /// of its windows on a thread of their own, a display's worth each, and told as `Handover` once
    /// back. Not taken with a transition asked, whose frame zero waits on this thread.
    fn compare_screen(&mut self, check: HandoverCheck, overlay: Whole) {
        if self.transition_waits() {
            return;
        }
        let Some(screen) = skylight::capture_screen(check.display).map(Whole) else {
            debug!("no picture of the screen for the handover check");
            return;
        };
        let done = self.done_tx.clone();
        let comparing = std::thread::Builder::new().name("handover".to_owned()).spawn(move || {
            let windows = compared(&check, &overlay, &screen);
            done.send(Done::Compared { tag: check.tag, windows });
        });
        if let Err(error) = comparing {
            warn!(%error, "no thread to compare the handover on");
        }
    }

    /// SkyLight reads the framebuffer, so a bar read while a transition was on its way, or began, may
    /// have the overlay in it, and is not kept: that transition's lift asks again.
    fn picture_bar(&self, display: Display) {
        let before = self.transitions.load(Ordering::Acquire);
        if transition_asked(before, self.applied_transitions) {
            return;
        }
        let (picture, strip) = bar_of(&bar_strip(display.frame), display.scale);
        let after = self.transitions.load(Ordering::Acquire);
        if !bar_read_kept(before, self.applied_transitions, after) {
            return;
        }
        let (Some(picture), Some(strip)) = (picture, strip) else {
            return;
        };
        self.shared.lock().unwrap().display.bar = Some((picture, strip));
    }

    fn finish(&mut self, done: Done) {
        match done {
            Done::Listed { target, prepared } => {
                self.shared.lock().unwrap().schedule.listed(target.window, prepared);
            }
            Done::Captured { window, picture } => {
                let (events, released) = {
                    let mut shared = self.shared.lock().unwrap();
                    let kept = shared.schedule.captured(window);
                    if let Some(picture) = picture.as_ref().filter(|_| kept.is_some()) {
                        debug!(
                            pid = window.pid,
                            idx = window.idx.get(),
                            covered = format!(
                                "{:.0}x{:.0}",
                                picture.coverage.covered.width, picture.coverage.covered.height
                            ),
                            window_size = format!(
                                "{:.0}x{:.0}",
                                picture.coverage.window.width, picture.coverage.window.height
                            ),
                            usable = picture.is_usable(),
                            "background snapshot landed"
                        );
                    }
                    let Shared { cache, delivered, schedule, .. } = &mut *shared;
                    let events = landing(cache, delivered, schedule, Instant::now(), window, kept, picture);
                    (events, !schedule.held())
                };
                self.tell(events);
                if released {
                    self.check_focused();
                }
            }
            Done::Desktop { display, picture } => {
                let mut shared = self.shared.lock().unwrap();
                let Shared { schedule, display: held, .. } = &mut *shared;
                let display_id = display.id;
                if land_desktop(schedule, held, display, picture) {
                    debug!(display_id, "desktop capture landed");
                }
            }
            Done::Compared { tag, windows } => self.tell(vec![Event::Handover { tag, windows }]),
        }
    }
}

/// `check`'s windows, each compared between the overlay's picture and the screen's, both drawn whole.
fn compared(check: &HandoverCheck, overlay: &Whole, screen: &Whole) -> Vec<(WindowId, HandoverDiff)> {
    let (Some(overlay), Some(screen)) = (rgba(&overlay.0), rgba(&screen.0)) else {
        debug!("a picture of the handover check could not be drawn");
        return Vec::new();
    };
    let (overlay, screen) = (overlay.at(check.overlay_origin, check.scale), screen.at(check.display.origin, check.scale));
    check
        .windows
        .iter()
        .filter_map(|&(window, frame, band)| Some((window, compare(&overlay, &screen, frame, band)?)))
        .collect()
}

#[cfg(test)]
mod tests {
    use objc2_core_foundation::{CGPoint, CGSize};
    use rini_geometry::space::ScreenPoint;

    use super::*;
    use crate::pictures::domain::handover::AFTER_LIFT;
    use crate::pictures::domain::watch::{Accept, CHANGE_EVERY, CHANGE_TRIES, Moment, Settle};
    use crate::pictures::platform::picture::test_picture;

    fn wid(idx: u32) -> WindowId { WindowId::new(1, idx) }

    fn sized(width: f64, height: f64) -> Picture { test_picture(ScreenSize::new(width, height)) }

    fn sliver() -> Picture {
        let mut picture = sized(859.0, 1081.0);
        picture.coverage.covered = ScreenSize::new(40.0, 1081.0);
        picture
    }

    fn display(id: u32, width: f64, height: f64) -> Display {
        Display {
            id,
            frame: ScreenRect::from_size(ScreenSize::new(width, height)),
            scale: 2.0,
        }
    }

    fn built_in() -> Display { display(1, 1728.0, 1117.0) }

    fn external() -> Display { display(2, 3008.0, 1692.0) }

    fn strip() -> ScreenRect { ScreenRect::from_size(ScreenSize::new(1728.0, 32.0)) }

    /// What every client hears of a background capture: the switcher redraws on `Taken`, a capture
    /// the cache refused is no news, and a closed window's capture is never put back in the cache.
    #[test]
    fn a_capture_the_cache_took_is_taken_and_one_it_refused_is_no_news() {
        let now = Instant::now();
        let mut cache = PictureCache::new();
        let mut delivered = Delivered::default();
        let mut schedule = Schedule::<(), (), ()>::default();
        let kept = || owed_as_focus(Vec::new());
        assert!(landing(&mut cache, &mut delivered, &mut schedule, now, wid(1), None, Some(sized(859.0, 1081.0))).is_empty());
        assert!(cache.get(wid(1)).is_none(), "forgotten meanwhile: not cached");
        assert!(landing(&mut cache, &mut delivered, &mut schedule, now, wid(1), kept(), None).is_empty(), "no picture");

        assert!(matches!(
            landing(&mut cache, &mut delivered, &mut schedule, now, wid(1), kept(), Some(sized(859.0, 1081.0))).as_slice(),
            [Event::Taken { window, .. }] if *window == wid(1)
        ));

        let mut blurred = sized(859.0, 1081.0);
        blurred.carries_blur = true;
        landing(&mut cache, &mut delivered, &mut schedule, now, wid(2), kept(), Some(blurred));
        assert!(
            landing(&mut cache, &mut delivered, &mut schedule, now, wid(2), kept(), Some(sized(859.0, 1081.0))).is_empty(),
            "refused for the blur it holds"
        );
        assert!(
            landing(&mut cache, &mut delivered, &mut schedule, now, wid(2), kept(), Some(sliver())).is_empty(),
            "a sliver refused is no news"
        );
    }

    fn tag(generation: u64) -> Tag {
        Tag {
            client: crate::pictures::domain::watch::Client(1),
            generation,
        }
    }

    fn frame() -> ScreenRect {
        ScreenRect::new(ScreenPoint::new(4.0, 32.0), ScreenSize::new(859.0, 1081.0))
    }

    fn owed_as_focus(focus: Vec<(Tag, ScreenRect)>) -> Option<Owed> { Some(Owed { focus, settles: false }) }

    /// The one ScreenCaptureKit capture of a clipped focus end is delivered to the client owed it as
    /// its focus change, once the cache has it; refused, it is delivered to no one.
    #[test]
    fn a_capture_owed_as_a_focus_change_is_delivered_when_the_cache_takes_it() {
        let now = Instant::now();
        let mut cache = PictureCache::new();
        let mut delivered = Delivered::default();
        let mut schedule = Schedule::<(), (), ()>::default();
        let events = landing(&mut cache, &mut delivered, &mut schedule, now, wid(1), owed_as_focus(vec![(tag(3), frame())]), Some(sized(859.0, 1081.0)));
        assert!(matches!(&events[..], [
            Event::Taken { .. },
            Event::Delivered { tag: t, why: Why::FocusChanged, pictures, .. },
        ] if *t == tag(3) && pictures.len() == 1 && pictures[0].2 == frame()));
        assert!(delivered.under(tag(3), wid(1)).is_some(), "recorded, so it can be kept as a look");

        let mut blurred = sized(859.0, 1081.0);
        blurred.carries_blur = true;
        cache.insert(wid(2), blurred);
        let events = landing(&mut cache, &mut delivered, &mut schedule, now, wid(2), owed_as_focus(vec![(tag(3), frame())]), Some(sized(859.0, 1081.0)));
        assert!(events.is_empty());
        assert!(delivered.under(tag(3), wid(2)).is_none());
    }

    /// A ScreenCaptureKit capture a repaint watch is owed is judged by its thumbprint once the cache
    /// has it: owed again until two in a row match, then delivered as the window landed; one the cache
    /// refuses loses the window.
    #[test]
    fn a_capture_owed_to_a_repaint_watch_is_delivered_as_landed_once_two_in_a_row_match() {
        let now = Instant::now();
        let cut = ScreenRect::new(ScreenPoint::new(-285.0, 32.0), ScreenSize::new(1148.0, 1081.0));
        let watched = |cache: &mut PictureCache<Picture>| {
            let mut schedule = Schedule::<(), (), ()>::default();
            schedule.set_display(built_in(), false, now);
            let landing = Watch::Landing {
                tag: tag(3),
                windows: vec![(target(1), cut)],
                behind: Vec::new(),
                slack: 20.0,
                settle: Settle::Repainted,
                until: now + Duration::from_millis(300),
            };
            schedule.watch(WatchId(1), landing, now);
            let _ = sky(schedule.next(now, |window| held(cache, window)));
            schedule.seen(now, &[Seen { window: wid(1), frame: Some(cut), capture: Capture::Missed }]);
            schedule
        };
        let came_back = |schedule: &mut Schedule<(), (), ()>| loop {
            match schedule.next(now, |_| None) {
                Next::List(targets) => targets.into_iter().for_each(|t| schedule.listed(t.window, Some(()))),
                Next::Capture { target, .. } => break schedule.captured(target.window),
                other => panic!("a capture: {other:?}"),
            }
        };
        let mut cache = PictureCache::new();
        let mut delivered = Delivered::default();
        let mut schedule = watched(&mut cache);
        let first = came_back(&mut schedule);
        let events = landing(&mut cache, &mut delivered, &mut schedule, now, wid(1), first, Some(sized(1148.0, 1081.0)));
        assert!(matches!(&events[..], [Event::Taken { .. }]), "{events:?}");
        let second = came_back(&mut schedule);
        let events = landing(&mut cache, &mut delivered, &mut schedule, now, wid(1), second, Some(sized(1148.0, 1081.0)));
        assert!(matches!(&events[..], [
            Event::Taken { .. },
            Event::Delivered { tag: t, why: Why::Landed, pictures, .. },
        ] if *t == tag(3) && pictures.len() == 1 && pictures[0].2 == cut), "{events:?}");
        assert!(delivered.under(tag(3), wid(1)).is_some());

        let mut cache = PictureCache::new();
        let mut blurred = sized(1148.0, 1081.0);
        blurred.carries_blur = true;
        cache.insert(wid(1), blurred);
        let mut schedule = watched(&mut cache);
        let owed = came_back(&mut schedule);
        let events = landing(&mut cache, &mut delivered, &mut schedule, now, wid(1), owed, Some(sized(1148.0, 1081.0)));
        assert!(matches!(&events[..], [Event::Lost { tag: t, window }] if *t == tag(3) && *window == wid(1)), "{events:?}");
    }

    fn target(idx: u32) -> PictureTarget {
        PictureTarget {
            window: wid(idx),
            server_id: WindowServerId::new(idx),
            size: ScreenSize::new(859.0, 1081.0),
        }
    }

    fn on_screen(generation: u64, moment: Moment, windows: &[u32]) -> OnScreen {
        OnScreen {
            tag: tag(generation),
            moment,
            strip: windows.iter().map(|idx| (target(*idx), frame())).collect(),
            background: Vec::new(),
            behind: Vec::new(),
        }
    }

    fn landing_watch(windows: &[u32], settle: Settle) -> Watch {
        Watch::Landing {
            tag: tag(1),
            windows: windows.iter().map(|idx| (target(*idx), frame())).collect(),
            behind: Vec::new(),
            slack: 20.0,
            settle,
            until: Instant::now() + Duration::from_secs(1),
        }
    }

    fn change_watch(idx: u32) -> Watch {
        Watch::Change {
            tag: tag(1),
            window: target(idx),
            end: frame(),
            losing: None,
            behind: Vec::new(),
            every: CHANGE_EVERY,
            tries: CHANGE_TRIES,
        }
    }

    /// The looks of a SkyLight round.
    fn sky<P, O>(next: Next<P, O>) -> Vec<Look> {
        match next {
            Next::Sky(sets) => sets.into_iter().flat_map(|set| set.looks).collect(),
            Next::Refresh(set) => set.looks,
            _ => panic!("not a SkyLight round"),
        }
    }

    /// Every window `looks` names, with the server frame `frame`.
    fn found(looks: &[Look], frame: Option<ScreenRect>) -> Vec<(WindowId, Option<CGRect>)> {
        looks.iter().map(|look| (look.target.window, frame.map(ScreenRect::to_cg))).collect()
    }

    /// One SkyLight round of `ask`, every window found at `frame()` and captured as `pictures` says.
    fn asked(
        schedule: &mut Schedule<(), (), ()>,
        cache: &mut PictureCache<Picture>,
        delivered: &mut Delivered<Picture>,
        ask: OnScreen,
        pictures: Vec<(WindowId, Picture)>,
    ) -> Vec<Event> {
        let now = Instant::now();
        schedule.ask_on_screen(ask);
        let looks = sky(schedule.next(now, |_| None));
        round(cache, delivered, schedule, now, found(&looks, Some(frame())), with_prints(&looks, pictures))
    }

    /// A round puts each picture to the cache before the schedule sees it: every sink hears of each
    /// one taken once, a covering one refused for the blurred one held is never delivered, and frame
    /// zero is delivered even with nothing taken.
    #[test]
    fn a_round_tells_the_schedule_only_of_what_the_cache_took() {
        let now = Instant::now();
        let mut schedule = Schedule::<(), (), ()>::default();
        schedule.set_display(built_in(), false, now);
        let mut cache = PictureCache::new();
        let mut delivered = Delivered::default();
        cache.insert(wid(2), blurred(0.5, now));
        let events = asked(
            &mut schedule,
            &mut cache,
            &mut delivered,
            on_screen(1, Moment::FrameZero, &[1, 2]),
            vec![(wid(1), flat(0.5, now)), (wid(2), flat(0.5, now))],
        );
        assert!(matches!(&events[..], [
            Event::Taken { window: taken, .. },
            Event::Delivered { why: Why::FrameZero, pictures, .. },
        ] if *taken == wid(1) && pictures.len() == 1 && pictures[0].0 == wid(1)));
        assert!(delivered.under(tag(1), wid(1)).is_some());
        assert!(delivered.under(tag(1), wid(2)).is_none(), "never delivered");

        schedule.ask_on_screen(on_screen(2, Moment::FrameZero, &[3]));
        let looks = sky(schedule.next(now, |_| None));
        let events = round(&mut cache, &mut delivered, &mut schedule, now, found(&looks, None), Vec::new());
        assert!(matches!(&events[..], [Event::Delivered { why: Why::FrameZero, pictures, .. }] if pictures.is_empty()));
    }

    /// A landing whose capture the cache refuses for the blurred picture it holds is lost at once, so
    /// the hold hears that its picture will not come; a refused change capture delivers nothing,
    /// records nothing as delivered, and finishes its watch.
    #[test]
    fn a_refused_landing_is_lost_and_a_refused_change_delivers_nothing() {
        let now = Instant::now();
        let mut schedule = Schedule::<(), (), ()>::default();
        schedule.set_display(built_in(), false, now);
        schedule.hold(Hold::Transition, now);
        let mut cache = PictureCache::new();
        let mut delivered = Delivered::default();
        cache.insert(wid(1), blurred(0.5, now));
        cache.insert(wid(2), blurred(0.5, now));
        schedule.watch(WatchId(1), landing_watch(&[1], Settle::Repainted), now);
        schedule.hint(Hint::Focused(wid(2)), now);
        schedule.watch(WatchId(2), change_watch(2), now);
        let looks = sky(schedule.next(now, |_| None));
        let captured = with_prints(&looks, vec![(wid(1), flat(0.1, now)), (wid(2), flat(0.1, now))]);
        let events = round(&mut cache, &mut delivered, &mut schedule, now, found(&looks, Some(frame())), captured);
        assert!(matches!(&events[..], [
            Event::Lost { tag: lost_tag, window: lost },
        ] if *lost_tag == tag(1) && *lost == wid(1)));
        assert!(delivered.under(tag(1), wid(2)).is_none());
        assert!(matches!(schedule.next(now, |_| None), Next::Idle), "nothing more is asked of either");
    }

    /// A final capture the cache refused was never delivered, so the next one showing the same is news.
    #[test]
    fn a_final_ask_after_a_refused_capture_counts_the_next_one_as_news() {
        let now = Instant::now();
        let mut schedule = Schedule::<(), (), ()>::default();
        schedule.set_display(built_in(), false, now);
        schedule.hold(Hold::Transition, now);
        let mut cache = PictureCache::new();
        let mut delivered = Delivered::default();
        let events = asked(
            &mut schedule,
            &mut cache,
            &mut delivered,
            on_screen(1, Moment::FrameZero, &[1]),
            vec![(wid(1), flat(0.5, now))],
        );
        assert!(matches!(&events[..], [Event::Taken { .. }, Event::Delivered { why: Why::FrameZero, .. }]));
        cache.insert(wid(1), blurred(0.5, now));

        let final_ask = || on_screen(1, Moment::Final, &[1]);
        let events = asked(&mut schedule, &mut cache, &mut delivered, final_ask(), vec![(wid(1), flat(0.1, now))]);
        assert!(events.is_empty(), "refused for the blurred one held");
        let events = asked(&mut schedule, &mut cache, &mut delivered, final_ask(), vec![(wid(1), blurred(0.1, now))]);
        assert!(matches!(&events[..], [
            Event::Taken { .. },
            Event::Delivered { why: Why::Final, pictures, .. },
        ] if pictures.len() == 1));
    }

    /// A delivery says when the service sent it: a round's at the `now` it is given once its captures
    /// are done, a focus change's when its capture came back. Each picture keeps when its capture
    /// started.
    #[test]
    fn a_delivery_carries_when_the_service_sent_it() {
        let taken = Instant::now();
        let sent = taken + Duration::from_millis(31);
        let mut schedule = Schedule::<(), (), ()>::default();
        schedule.set_display(built_in(), false, taken);
        let mut cache = PictureCache::new();
        let mut delivered = Delivered::default();
        schedule.ask_on_screen(on_screen(1, Moment::FrameZero, &[1]));
        let looks = sky(schedule.next(taken, |_| None));
        let captured = with_prints(&looks, vec![(wid(1), flat(0.5, taken))]);
        let events = round(&mut cache, &mut delivered, &mut schedule, sent, found(&looks, Some(frame())), captured);
        assert!(matches!(&events[..], [
            Event::Taken { .. },
            Event::Delivered { why: Why::FrameZero, sent: at, pictures, .. },
        ] if *at == sent && pictures[0].1.taken == taken));

        let owed = owed_as_focus(vec![(tag(3), frame())]);
        let events = landing(&mut cache, &mut delivered, &mut schedule, sent, wid(2), owed, Some(flat(0.5, taken)));
        assert!(matches!(&events[..], [
            Event::Taken { .. },
            Event::Delivered { why: Why::FocusChanged, sent: at, .. },
        ] if *at == sent));
    }

    /// A window is captured only at a server frame one of its accepts takes on the display held; with
    /// no display held, none is.
    #[test]
    fn a_window_is_wanted_only_where_an_accept_takes_its_frame_on_the_display_held() {
        let look = |idx, accepts| Look {
            target: target(idx),
            accepts,
            thumbprint: false,
        };
        let off = ScreenRect::new(ScreenPoint::new(1700.0, 32.0), ScreenSize::new(859.0, 1081.0));
        let looks = [
            look(1, vec![Accept::Origin { at: frame().origin }]),
            look(2, vec![Accept::OnDisplay]),
            look(3, vec![Accept::OnDisplay]),
            look(4, vec![Accept::Near { end: off, slack: 20.0 }, Accept::OnDisplay]),
        ];
        let frames =
            [Some(frame()), None, Some(off), Some(frame())].map(|frame| frame.map(ScreenRect::to_cg));
        let wanted_now: Vec<(WindowId, WindowServerId, ScreenRect)> =
            wanted(&looks, &frames, Some(built_in()))
                .iter()
                .map(|window| (window.window, window.server_id, ScreenRect::from_cg(window.frame)))
                .collect();
        assert_eq!(
            wanted_now,
            vec![
                (wid(1), WindowServerId::new(1), frame()),
                (wid(4), WindowServerId::new(4), frame())
            ],
            "with no frame, or off the display, a window is not wanted; one accept is enough"
        );
        assert!(wanted(&looks, &frames, None).is_empty(), "no display held");
    }

    /// Every picture captured is band printed, and thumbprinted only where a look at its window asks.
    #[test]
    fn a_capture_is_thumbprinted_only_where_its_look_asks() {
        let now = Instant::now();
        let look = |idx, thumbprint| Look {
            target: target(idx),
            accepts: vec![Accept::OnDisplay],
            thumbprint,
        };
        let printed: Vec<(WindowId, bool, bool)> =
            with_prints(&[look(1, true), look(2, false)], vec![(wid(1), flat(0.5, now)), (wid(2), flat(0.5, now))])
                .into_iter()
                .map(|(window, _, prints)| {
                    (window, prints.is_some(), prints.is_some_and(|prints| prints.thumb.is_some()))
                })
                .collect();
        assert_eq!(printed, vec![(wid(1), true, true), (wid(2), true, false)]);
    }

    /// A display change leaves nothing of the display left behind, its desktop's windows included,
    /// and a bar no longer pictured goes on its own.
    #[test]
    fn a_display_change_empties_what_is_held_and_a_bar_turned_off_drops_only_the_bar() {
        let now = Instant::now();
        let mut schedule = Schedule::<(), (), ()>::default();
        let mut held = DisplayPictures::default();
        set_display(&mut schedule, &mut held, built_in(), true, now);
        held.desktop = Some(sized(1728.0, 1117.0));
        held.bar = Some((sized(1728.0, 32.0), strip()));
        held.desktop_windows = Some(DesktopBackdrop::default());

        set_display(&mut schedule, &mut held, built_in(), false, now);
        assert!(held.desktop.is_some(), "the desktop stays");
        assert!(held.bar.is_none(), "the bar goes");

        held.bar = Some((sized(1728.0, 32.0), strip()));
        set_display(&mut schedule, &mut held, external(), true, now);
        assert!(held.desktop.is_none() && held.bar.is_none() && held.desktop_windows.is_none());
    }

    /// A change of display asks no pictures of its own.
    #[test]
    fn a_display_change_warms_nothing() {
        let (mut service, shared) = detached_service();
        service.apply(Command::SetDisplay { display: built_in(), picture_bar: false });
        service.apply(Command::SetDisplay { display: external(), picture_bar: false });
        assert_eq!(shared.lock().unwrap().schedule.owed().count(), 0);
    }

    /// The reported case: a render of the external display, landing after the overlay moved to the
    /// built-in one, was drawn at its own size and showed its top-left corner. A render is held only
    /// at the size of the display it was asked of, and only while that display is held.
    #[test]
    fn a_render_is_held_only_for_the_display_held_and_only_at_its_size() {
        let now = Instant::now();
        let mut schedule = Schedule::<(), (), ()>::default();
        let mut held = DisplayPictures::default();
        set_display(&mut schedule, &mut held, built_in(), false, now);
        let _ = schedule.next(now, |_| None);
        assert!(!land_desktop(&mut schedule, &mut held, built_in(), Some(sized(3008.0, 1692.0))));
        assert!(held.desktop.is_none(), "the wrong size");

        set_display(&mut schedule, &mut held, external(), false, now);
        assert!(!land_desktop(&mut schedule, &mut held, built_in(), Some(sized(1728.0, 1117.0))));
        assert!(held.desktop.is_none(), "the display left");

        let _ = schedule.next(now, |_| None);
        assert!(land_desktop(&mut schedule, &mut held, external(), Some(sized(3008.0, 1692.0))));
        assert!(held.desktop.is_some());
    }

    /// A transition is dressed only with the pictures of its own display, and with no bar once the
    /// bar is off the screen.
    #[test]
    fn a_transition_is_dressed_only_with_its_own_displays_pictures() {
        let held = DisplayPictures {
            desktop: Some(sized(1728.0, 1117.0)),
            bar: Some((sized(1728.0, 32.0), strip())),
            desktop_windows: None,
        };
        let (desktop, bar) = held.for_display(Some(built_in()), built_in(), true);
        assert!(desktop.is_some() && bar.is_some());
        let (desktop, bar) = held.for_display(Some(built_in()), external(), true);
        assert!(desktop.is_none() && bar.is_none(), "another display");
        let (desktop, bar) = held.for_display(None, built_in(), true);
        assert!(desktop.is_none() && bar.is_none(), "no display held");
        let (desktop, bar) = held.for_display(Some(built_in()), built_in(), false);
        assert!(desktop.is_some() && bar.is_none(), "the bar has gone from the screen");
    }

    /// SkyLight reads the framebuffer: a bar read with a transition asked and not yet applied, or
    /// asked while it ran, may have the overlay in it.
    #[test]
    fn a_bar_read_is_kept_only_when_no_transition_was_on_its_way() {
        assert!(bar_read_kept(3, 3, 3), "quiet");
        assert!(!bar_read_kept(4, 3, 4), "asked, not yet applied");
        assert!(!bar_read_kept(3, 3, 4), "began during the read");
    }

    /// The handle counts a transition as it is asked, ahead of its hold: until the thread applies it,
    /// the thread starts no read of the screen for a handover check.
    #[test]
    fn a_transition_is_asked_until_the_thread_applies_it() {
        assert!(transition_asked(4, 3));
        assert!(!transition_asked(4, 4));
    }

    /// The thread's side of a service with no thread, ScreenCaptureKit or sink behind it.
    fn detached_service() -> (Service, Arc<Mutex<Shared>>) {
        let (service, shared, _commands) = detached_service_and_commands();
        (service, shared)
    }

    /// The same, with the handle's side of its command channel.
    fn detached_service_and_commands() -> (Service, Arc<Mutex<Shared>>, Sender<Command>) {
        let (commands, requests) = channel::channel();
        let (done_tx, done) = channel::channel();
        let shared = Arc::new(Mutex::new(Shared::default()));
        let service = Service {
            requests,
            done,
            done_tx,
            shared: shared.clone(),
            transitions: Arc::default(),
            applied_transitions: 0,
            looks_checked: HashMap::new(),
            sinks: Vec::new(),
        };
        (service, shared, commands)
    }

    /// A window drawn grey at `level`, 40x30 pixels.
    fn flat(level: f64, taken: Instant) -> Picture {
        use objc2_core_graphics::CGContext;
        let ctx = crate::pictures::platform::bitmap::rgba_bitmap_context(40, 30).unwrap();
        CGContext::set_rgb_fill_color(Some(&ctx), level, level, level, 1.0);
        CGContext::fill_rect(Some(&ctx), CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(40.0, 30.0)));
        Picture {
            image: Pixels::Bitmap(objc2_core_graphics::CGBitmapContextCreateImage(Some(&ctx)).unwrap()),
            taken,
            ..sized(20.0, 15.0)
        }
    }

    /// The same, carrying its blur, as a baked capture of a window with a material does.
    fn blurred(level: f64, taken: Instant) -> Picture {
        Picture {
            carries_blur: true,
            ..flat(level, taken)
        }
    }

    /// A client keeps as a window's focused look only a picture it was delivered under the tag it
    /// names; the look stays through a newer capture of the same content, goes with one of different
    /// content, and is judged only once no transition holds the service.
    #[test]
    fn a_focused_look_is_kept_by_its_tag_and_dropped_when_newer_content_differs() {
        let (mut service, shared) = detached_service();
        let start = Instant::now();
        shared.lock().unwrap().delivered.record(tag(2), wid(1), flat(0.5, start));
        service.apply(Command::KeepFocused(tag(1), wid(1)));
        assert!(shared.lock().unwrap().cache.focused(wid(1)).is_none(), "not delivered under that tag");
        service.apply(Command::KeepFocused(tag(2), wid(1)));
        assert!(shared.lock().unwrap().cache.focused(wid(1)).is_some());

        service.apply(Command::Hold(Hold::Transition));
        shared.lock().unwrap().cache.insert(wid(1), flat(0.5, start + Duration::from_millis(5)));
        service.apply(Command::Hold(Hold::Released));
        assert!(shared.lock().unwrap().cache.focused(wid(1)).is_some(), "the same content");

        service.apply(Command::Hold(Hold::Transition));
        shared.lock().unwrap().cache.insert(wid(1), flat(0.1, start + Duration::from_millis(10)));
        service.apply(Command::Hold(Hold::Settling));
        assert!(shared.lock().unwrap().cache.focused(wid(1)).is_some(), "not judged while held");
        service.apply(Command::Hold(Hold::Released));
        assert!(shared.lock().unwrap().cache.focused(wid(1)).is_none(), "content changed");
    }

    /// A window that is gone takes everything known of it: its pictures, its focused look, what it was
    /// delivered, and its knowledge, whose id may come back on another window.
    #[test]
    fn a_forgotten_window_leaves_nothing_behind() {
        let (mut service, shared) = detached_service();
        {
            let mut shared = shared.lock().unwrap();
            shared.cache.insert(wid(1), sized(859.0, 1081.0));
            shared.cache.keep_focused(wid(1), sized(859.0, 1081.0));
            shared.delivered.record(tag(1), wid(1), sized(859.0, 1081.0));
            shared.learned.saw_alpha(WindowServerId::from(wid(1)), true);
        }
        service.apply(Command::Forget(wid(1)));
        let shared = shared.lock().unwrap();
        assert!(shared.cache.get(wid(1)).is_none());
        assert!(shared.cache.focused(wid(1)).is_none());
        assert!(shared.delivered.under(tag(1), wid(1)).is_none());
        assert_eq!(shared.learned.get(WindowServerId::from(wid(1))), Knowledge::default());
    }

    /// A focused look that is a region is kept as a copy at a release too.
    #[test]
    fn a_release_keeps_a_focused_look_that_is_a_region_as_a_copy() {
        let (mut service, shared) = detached_service();
        shared.lock().unwrap().cache.keep_focused(wid(1), crate::pictures::platform::picture::test_region());
        service.apply(Command::Hold(Hold::Released));
        assert!(matches!(shared.lock().unwrap().cache.focused(wid(1)).unwrap().image, Pixels::Bitmap(_)));
    }

    /// One SkyLight round on the service at `at`: the looks the schedule hands out past the desktop and
    /// the bar, due on their own, every window found at `frame()` and captured as `pictures` says.
    fn service_round(service: &mut Service, shared: &Arc<Mutex<Shared>>, at: Instant, pictures: Vec<(WindowId, Picture)>) {
        let looks = {
            let schedule = &mut shared.lock().unwrap().schedule;
            let mut next = schedule.next(at, |_| None);
            while matches!(next, Next::Desktop(_) | Next::Bar(_)) {
                next = schedule.next(at, |_| None);
            }
            sky(next)
        };
        let captured = with_prints(&looks, pictures);
        service.put_round(Looking::Watches, found(&looks, Some(frame())), captured, None, false);
    }

    fn delivered_of(shared: &Arc<Mutex<Shared>>, idx: u32) -> bool {
        shared.lock().unwrap().delivered.under(tag(1), wid(idx)).is_some()
    }

    /// What a round learned is kept, and the desktop's windows listed with it unless a baked composite
    /// came back another shape, which a desktop window gone from it makes: they are listed afresh the
    /// next time. A round that captured nothing writes nothing back.
    #[test]
    fn a_round_keeps_what_it_learned_and_drops_the_desktop_windows_when_stale() {
        let (mut service, shared) = detached_service();
        let window = WindowServerId::new(1);
        let mut learned = Learned::default();
        learned.saw_alpha(window, true);
        let listed = || {
            Some(DesktopBackdrop {
                windows: vec![WindowServerId::new(7)],
                ..DesktopBackdrop::default()
            })
        };
        service.put_round(Looking::Watches, Vec::new(), Vec::new(), Some((learned.clone(), listed())), false);
        service.put_round(Looking::Watches, Vec::new(), Vec::new(), None, false);
        {
            let shared = shared.lock().unwrap();
            assert_eq!(shared.learned.get(window).translucent, Some(true));
            assert!(shared.display.desktop_windows.is_some());
        }
        service.put_round(Looking::Watches, Vec::new(), Vec::new(), Some((learned, listed())), true);
        assert!(shared.lock().unwrap().display.desktop_windows.is_none(), "stale");
    }

    /// A round the cache took a picture from, with no transition holding the service, keeps its
    /// regions as copies at once; held, or with nothing taken, a region stays one until the release.
    #[test]
    fn a_round_at_rest_keeps_regions_as_copies_once_the_cache_took_a_picture() {
        let region = crate::pictures::platform::picture::test_region;
        let is_region = |shared: &Arc<Mutex<Shared>>| {
            matches!(shared.lock().unwrap().cache.get(wid(1)).unwrap().image, Pixels::Region { .. })
        };

        let (mut service, shared) = detached_service();
        service.put_round(Looking::Watches, Vec::new(), vec![(wid(1), region(), None)], None, false);
        assert!(!is_region(&shared), "kept as a copy");

        let (mut service, shared) = detached_service();
        service.apply(Command::Hold(Hold::Transition));
        service.put_round(Looking::Watches, Vec::new(), vec![(wid(1), region(), None)], None, false);
        assert!(is_region(&shared), "held");

        let (mut service, shared) = detached_service();
        shared.lock().unwrap().cache.insert(
            wid(1),
            Picture {
                carries_blur: true,
                ..region()
            },
        );
        service.put_round(Looking::Watches, Vec::new(), vec![(wid(1), region(), None)], None, false);
        assert!(is_region(&shared), "refused: nothing taken");
    }

    /// A watched landing that has to repaint is never settled by its first capture, however it
    /// differs from the picture the cache holds: an unfolded window's first capture at its new size
    /// differed from its old picture while half of it was blank. The capture after it, matching it,
    /// is delivered.
    #[test]
    fn a_repainting_landing_is_delivered_once_two_captures_match_and_never_for_differing_from_the_picture_held() {
        let now = Instant::now();
        let (mut service, shared) = detached_service();
        {
            let mut shared = shared.lock().unwrap();
            shared.schedule.set_display(built_in(), false, now);
            shared.cache.insert(wid(1), flat(0.5, now));
            shared.cache.insert(wid(2), flat(0.5, now));
        }
        service.apply(Command::Watch(WatchId(1), landing_watch(&[1, 2], Settle::Repainted)));
        service_round(&mut service, &shared, Instant::now(), vec![(wid(1), flat(0.1, now)), (wid(2), flat(0.5, now))]);
        assert!(!delivered_of(&shared, 1), "differs from the picture held");
        assert!(!delivered_of(&shared, 2), "matches the picture held");
        let next = Instant::now() + Duration::from_millis(50);
        service_round(&mut service, &shared, next, vec![(wid(1), flat(0.1, next)), (wid(2), flat(0.5, next))]);
        assert!(delivered_of(&shared, 1), "two captures in a row match");
        assert!(delivered_of(&shared, 2), "two captures in a row match");
    }

    /// The reported shape of a click at rest: the picture held of a window is old and its band differs
    /// from the window's now, a title changed since; the refresh's first capture comes before the app
    /// repaints, and a later one shows the turn. Nothing is put while it polls, so the cache holds the
    /// picture from before until the window's look settles, at a capture like the one before it, and
    /// then the capture it settled on, which every sink hears of once.
    #[test]
    fn a_refresh_puts_the_capture_its_window_settled_on_once_however_the_picture_held_before_differs() {
        let now = Instant::now();
        let (mut service, shared) = detached_service();
        let told = Arc::new(Mutex::new(Vec::new()));
        let heard = told.clone();
        service.sinks = vec![Box::new(move |event| {
            if let Event::Taken { window, picture } = event {
                heard.lock().unwrap().push((window, picture.taken));
            }
        })];
        {
            let mut shared = shared.lock().unwrap();
            shared.schedule.set_display(built_in(), false, now);
            shared.cache.insert(wid(1), flat(0.9, now));
        }
        service.apply(Command::Refresh(vec![target(1)]));
        let later = Instant::now() + Duration::from_secs(1);
        let refreshed = |service: &mut Service, picture: Picture| {
            let looks = sky(shared.lock().unwrap().schedule.next(later, |_| None));
            let captured = with_prints(&looks, vec![(wid(1), picture)]);
            service.put_round(Looking::Refresh, found(&looks, Some(frame())), captured, None, false);
        };
        let owed = || shared.lock().unwrap().schedule.owed().collect::<Vec<_>>();
        let held = || shared.lock().unwrap().cache.get(wid(1)).map(|held| held.taken);
        refreshed(&mut service, flat(0.5, now + Duration::from_millis(1)));
        assert_eq!(owed(), vec![wid(1)], "a first capture is never settled");
        refreshed(&mut service, flat(0.1, now + Duration::from_millis(2)));
        assert_eq!(owed(), vec![wid(1)], "turned, and not settled yet");
        assert_eq!(held(), Some(now), "nothing put while it polls");
        let settled = now + Duration::from_millis(3);
        refreshed(&mut service, flat(0.1, settled));
        assert!(owed().is_empty(), "settled");
        assert_eq!(held(), Some(settled));
        assert_eq!(*told.lock().unwrap(), vec![(wid(1), settled)], "told once");
    }

    /// A refresh's capture the cache would refuse, one without the blur of the blurred picture held at
    /// the same size, ends the refresh with nothing put: every later capture there is refused too.
    #[test]
    fn a_refresh_the_cache_would_refuse_ends_with_nothing_put() {
        let now = Instant::now();
        let (mut service, shared) = detached_service();
        {
            let mut shared = shared.lock().unwrap();
            shared.schedule.set_display(built_in(), false, now);
            shared.cache.insert(wid(1), blurred(0.5, now));
        }
        service.apply(Command::Refresh(vec![target(1)]));
        let later = Instant::now() + Duration::from_secs(1);
        let looks = sky(shared.lock().unwrap().schedule.next(later, |_| None));
        let captured = with_prints(&looks, vec![(wid(1), flat(0.1, later))]);
        service.put_round(Looking::Refresh, found(&looks, Some(frame())), captured, None, false);
        let shared = shared.lock().unwrap();
        assert!(shared.schedule.owed().next().is_none());
        assert_eq!(shared.cache.get(wid(1)).map(|held| (held.taken, held.carries_blur)), Some((now, true)));
    }

    /// No call is made while a command is queued: the thread applies it first, so a hold that arrives
    /// during a call is in force before the next. It cancels a refresh due then, and the release asks
    /// again for it, looked at once the settle has passed.
    #[test]
    fn no_call_is_made_while_a_command_is_queued() {
        use crate::pictures::domain::schedule::SETTLE_BEFORE_STARTING;
        let (mut service, shared, commands) = detached_service_and_commands();
        shared.lock().unwrap().schedule.refresh(vec![target(1)], Instant::now() - SETTLE_BEFORE_STARTING);
        commands.send(Command::Hold(Hold::Transition));
        assert_eq!(service.make_calls(), Some(Duration::ZERO), "the queue first");
        while let Ok((_, command)) = service.requests.try_recv() {
            service.apply(command);
        }
        assert_eq!(service.make_calls(), None, "held: nothing looked at, nothing to wake for");
        service.apply(Command::Hold(Hold::Released));
        let wait = service.make_calls().expect("asked again");
        assert!(wait > Duration::ZERO && wait <= SETTLE_BEFORE_STARTING, "{wait:?}");
        assert_eq!(shared.lock().unwrap().schedule.owed().collect::<Vec<_>>(), vec![wid(1)]);
    }

    /// An ask on screen, a stop and a focus hint reach the schedule as they are applied.
    #[test]
    fn an_ask_on_screen_a_stop_and_a_focus_hint_reach_the_schedule() {
        let (mut service, shared) = detached_service();
        let next = || shared.lock().unwrap().schedule.next(Instant::now(), |_| None);
        let looked = |next| sky(next).into_iter().map(|look| look.target.window).collect::<Vec<_>>();

        service.apply(Command::AskOnScreen(on_screen(1, Moment::FrameZero, &[1, 2])));
        assert_eq!(looked(next()), vec![wid(1), wid(2)]);

        service.apply(Command::Watch(WatchId(1), landing_watch(&[3], Settle::AsSoonAsThere)));
        assert_eq!(looked(next()), vec![wid(3)]);
        service.apply(Command::Stop(WatchId(1)));
        assert!(matches!(next(), Next::Idle), "stopped");

        service.apply(Command::Watch(WatchId(2), change_watch(4)));
        assert!(matches!(next(), Next::Idle), "no focus reported");
        service.apply(Command::Hint(Hint::Focused(wid(4))));
        assert_eq!(looked(next()), vec![wid(4)]);
        let settled = Instant::now() + crate::pictures::domain::knowledge::ACTIVE_SETTLED;
        assert!(shared.lock().unwrap().learned.active(wid(4), Some(1), settled), "what a material is measured by");
    }

    /// Every handle method that asks sends its command, in order; the reads are of what is shared.
    #[test]
    fn the_handle_sends_each_ask_and_reads_what_is_held() {
        let (pictures, mut commands) = Pictures::detached();
        pictures.ask_on_screen(on_screen(1, Moment::Final, &[1]));
        pictures.retarget(WatchId(3), change_watch(1));
        pictures.stop(WatchId(3));
        pictures.hint(Hint::Placed(wid(1)));
        pictures.keep_focused(tag(2), wid(1));
        pictures.check_handover(check());
        pictures.refresh(vec![target(2)]);
        let sent: Vec<Command> =
            std::iter::from_fn(|| commands.try_recv().ok().map(|(_, command)| command)).collect();
        assert!(matches!(&sent[..], [
            Command::AskOnScreen(ask),
            Command::Retarget(WatchId(3), Watch::Change { .. }),
            Command::Stop(WatchId(3)),
            Command::Hint(Hint::Placed(placed)),
            Command::KeepFocused(kept, focused),
            Command::CheckHandover(checked),
            Command::Refresh(refreshed),
        ] if ask.moment == Moment::Final && *placed == wid(1) && *kept == tag(2) && *focused == wid(1)
            && *checked == check() && *refreshed == vec![target(2)]));

        let now = Instant::now();
        {
            let mut shared = pictures.lock();
            shared.cache.keep_focused(wid(1), flat(0.5, now));
            shared.learned.saw_alpha(WindowServerId::from(wid(1)), true);
        }
        assert_eq!(pictures.focused(wid(1)).map(|look| look.taken), Some(now));
        assert!(pictures.focused(wid(2)).is_none());
        assert_eq!(pictures.knowledge(WindowServerId::from(wid(1))).translucent, Some(true));
    }

    fn check() -> HandoverCheck {
        HandoverCheck {
            tag: tag(4),
            overlay: WindowServerId::new(77),
            overlay_origin: ScreenPoint::new(0.0, 32.0),
            display: built_in().frame,
            scale: 2.0,
            windows: vec![(wid(1), frame(), 0.0)],
        }
    }

    /// A press waits on whatever the thread does before it applies the press's hold, so with a
    /// transition asked the overlay's picture is not taken: its client is told, and nothing is compared.
    #[test]
    fn the_overlay_is_not_pictured_with_a_transition_asked() {
        let (mut service, shared) = detached_service();
        shared.lock().unwrap().schedule.check_handover(check());
        let next = shared.lock().unwrap().schedule.next(Instant::now(), |_| None);
        let Next::Overlay(overlay) = next else { panic!("the overlay is the next call") };
        service.transitions.fetch_add(1, Ordering::AcqRel);
        service.take_overlay(overlay);
        let next = shared.lock().unwrap().schedule.next(Instant::now() + AFTER_LIFT, |_| None);
        assert!(matches!(next, Next::Idle), "nothing to compare");
    }

    /// Watch ids are the handle's, handed out at once, and the watch goes to the thread after them.
    #[test]
    fn a_watch_is_named_by_the_handle_and_sent_on() {
        let (pictures, mut commands) = Pictures::detached();
        let watch = || Watch::Live {
            tag: tag(1),
            windows: Vec::new(),
            every: Duration::from_millis(16),
            after: None,
            initial: None,
        };
        let first = pictures.watch(watch());
        let second = pictures.clone().watch(watch());
        assert_ne!(first, second, "clones share the count");
        assert!(matches!(commands.try_recv(), Ok((_, Command::Watch(id, _))) if id == first));
        assert!(matches!(commands.try_recv(), Ok((_, Command::Watch(id, _))) if id == second));
    }

    /// No region stays past a release: each the cache holds is replaced by a copy of its own, stamped
    /// as it was, a picture that is not a region is left as it is, and what each client was delivered
    /// is dropped, since nothing names it after its flight.
    #[test]
    fn a_release_keeps_every_region_as_a_copy_and_drops_what_was_delivered() {
        let (mut service, shared) = detached_service();
        let region = crate::pictures::platform::picture::test_region();
        let taken = region.taken;
        let tag = Tag { client: crate::pictures::domain::watch::Client(1), generation: 3 };
        shared.lock().unwrap().delivered.record(tag, wid(1), region.clone());
        shared.lock().unwrap().cache.insert(wid(1), region);
        shared.lock().unwrap().cache.insert(wid(2), sized(859.0, 1081.0));
        service.apply(Command::Hold(Hold::Transition));
        assert!(matches!(
            shared.lock().unwrap().cache.get(wid(1)).unwrap().image,
            Pixels::Region { .. }
        ), "held: still a region");
        assert!(shared.lock().unwrap().delivered.under(tag, wid(1)).is_some(), "held: still delivered");
        service.apply(Command::Hold(Hold::Released));
        let shared = shared.lock().unwrap();
        let kept = shared.cache.get(wid(1)).unwrap();
        assert!(matches!(kept.image, Pixels::Bitmap(_)));
        assert_eq!(kept.taken, taken);
        assert!(matches!(shared.cache.get(wid(2)).unwrap().image, Pixels::Bitmap(_)));
        assert!(shared.delivered.under(tag, wid(1)).is_none(), "its composite goes with it");
    }

    /// The switcher draws what `usable_many` returns, in its own order, and never a sliver.
    #[test]
    fn usable_many_keeps_the_order_asked_and_leaves_out_what_is_not_worth_drawing() {
        let (pictures, _commands) = Pictures::detached();
        {
            let mut shared = pictures.lock();
            shared.cache.insert(wid(1), sized(859.0, 1081.0));
            shared.cache.insert(wid(2), sliver());
            shared.cache.insert(wid(3), sized(1720.0, 1081.0));
        }
        let drawn: Vec<WindowId> = pictures
            .usable_many(&[wid(3), wid(2), wid(4), wid(1)])
            .into_iter()
            .map(|(window, _)| window)
            .collect();
        assert_eq!(drawn, vec![wid(3), wid(1)]);
    }
}
