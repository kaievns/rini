//! The pictures service: the thread that takes every background picture of a window, the desktop and
//! the bar, the cache those pictures land in, and the handle every client holds.
//!
//! One thread, `pictures`, runs the schedule (`crate::pictures::domain::schedule`). ScreenCaptureKit is
//! asked from it and its completions post back to it; the bar is composited on it. A client asks,
//! holds and forgets through a channel, reads from any thread through one lock, and hears of what lands
//! through the sink it was started with. See "The service" in `src/pictures/docs/README.md`.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use objc2_core_foundation::{CFRetained, CGRect, CGSize};
use objc2_core_graphics::CGImage;
use rini_core::ids::WindowId;
use rini_runloop::channel::{self, Receiver, Sender};
use tracing::{debug, warn};

use crate::pictures::domain::cache::PictureCache;
use crate::pictures::domain::coverage::{is_backdrop_worth_drawing, spans_display};
use crate::pictures::domain::schedule::{
    Ask, Display, Held, Hold, Next, Priority, Schedule, wants_warm,
};
use crate::pictures::domain::target::{PictureTarget, synthetic_window_id};
use crate::pictures::event::Event;
use crate::pictures::platform::desktop::{BarStrip, bar_strip, desktop_backdrop_windows};
use crate::pictures::platform::edge_dressing::EdgeDressing;
use crate::pictures::platform::picture::{Picture, Pixels, stand_in_picture, stand_in_image};
use crate::pictures::platform::screencapturekit::{self, Prepared};
use crate::pictures::platform::skylight::capture_list_via_skylight;

/// What a client asks of the service.
#[derive(Debug)]
pub enum Command {
    Ask(Ask),
    Hold(Hold),
    Forget(WindowId),
    SetDisplay { display: Display, picture_bar: bool },
    /// Every window on screen on the display, pictured in the background: the debug command's warm.
    WarmVisible,
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
}

/// Where a client hears of what lands. Called on the service's thread.
pub type Sink = Box<dyn Fn(Event) + Send>;

/// What a transition on one display is dressed with: the desktop behind its tiles and the bar over
/// them.
pub struct Dressing {
    pub desktop: Option<Picture>,
    pub bar: Option<Picture>,
    /// Where the bar sits, in global coordinates. With no picture, the picture already drawn stays.
    pub strip: Option<CGRect>,
}

/// The handle to the service. Any thread may hold one.
#[derive(Clone)]
pub struct Pictures {
    commands: Sender<Command>,
    shared: Arc<Mutex<Shared>>,
    /// Bumped by every `hold(Hold::Transition)` before it is sent: the overlay can go up while the
    /// service is reading the bar off the framebuffer, before the hold has reached it.
    transitions: Arc<AtomicU64>,
}

#[derive(Default)]
struct Shared {
    cache: PictureCache<Picture>,
    schedule: Schedule<Prepared>,
    /// The pictures of the schedule's display, which belong to it alone.
    display: DisplayPictures,
    stand_in: Option<StandIn>,
}

/// The pictures of the display the overlay flies on. Which display that is, the schedule holds.
#[derive(Default)]
struct DisplayPictures {
    /// The desktop as ScreenCaptureKit rendered it: the only source that reliably has the wallpaper.
    desktop: Option<Picture>,
    /// The last usable picture of the bar, and the strip it covers.
    bar: Option<(Picture, CGRect)>,
}

impl DisplayPictures {
    /// What is held for a transition on `display`: nothing for any display but the one held, and no
    /// bar once none is on screen.
    fn for_display(
        &self,
        held: Option<Display>,
        display: Display,
        bar_on_screen: bool,
    ) -> (Option<Picture>, Option<(Picture, CGRect)>) {
        if held != Some(display) {
            return (None, None);
        }
        (self.desktop.clone(), self.bar.clone().filter(|_| bar_on_screen))
    }
}

/// The overlay now flies on `display`. A change leaves nothing of the display left behind; a bar
/// no longer pictured is dropped. True when the display changed.
fn set_display<P>(
    schedule: &mut Schedule<P>,
    held: &mut DisplayPictures,
    display: Display,
    picture_bar: bool,
    now: Instant,
) -> bool {
    let changed = schedule.set_display(display, picture_bar, now);
    if changed {
        *held = DisplayPictures::default();
    } else if !picture_bar {
        held.bar = None;
    }
    changed
}

/// A render of `display`'s desktop came back. Held only when it spans the display it was asked of
/// and that display is still the one held: a render of the external drawn behind a flight on the
/// built-in display shows its top-left corner, zoomed.
fn land_desktop<P>(
    schedule: &mut Schedule<P>,
    held: &mut DisplayPictures,
    display: Display,
    picture: Option<Picture>,
) -> bool {
    let size = (display.frame.size.width, display.frame.size.height);
    let picture = picture.filter(|picture| spans_display(picture.coverage.covered, size));
    let kept = schedule.desktop_rendered(display, picture.as_ref().map(|picture| picture.taken));
    if kept {
        held.desktop = picture;
    }
    kept
}

/// What a background capture that came back tells the clients, once the cache has had it: `Taken`
/// when its pixels are now the ones held, `Refused` when it covered the window and the cache kept the
/// blurred one it holds, nothing for a sliver, an empty capture, or a window forgotten meanwhile.
fn landing(
    cache: &mut PictureCache<Picture>,
    window: WindowId,
    kept: bool,
    picture: Option<Picture>,
) -> Option<Event> {
    let picture = picture.filter(|_| kept)?;
    let usable = picture.is_usable();
    if cache.insert(window, picture.clone()) {
        Some(Event::Taken { window, picture })
    } else {
        usable.then_some(Event::Refused { window })
    }
}

/// Whether a bar read is free of the overlay: no transition was asked before it that the thread had
/// not yet applied, and none was asked while it ran.
fn bar_read_kept(asked_before: u64, applied: u64, asked_after: u64) -> bool {
    asked_before == applied && asked_after == asked_before
}

/// The stand-in's pixels, drawn once per scale.
struct StandIn {
    scale: f64,
    image: CFRetained<CGImage>,
}

// SAFETY: CGImage is immutable, like `Pixels::Bitmap`.
unsafe impl Send for StandIn {}

impl Pictures {
    /// Starts the service on its own thread. Every sink hears of every picture that lands.
    pub fn start(sinks: Vec<Sink>) -> Self {
        let (commands, requests) = channel::channel();
        let (done_tx, done) = channel::channel();
        let pictures = Self {
            commands,
            shared: Arc::default(),
            transitions: Arc::default(),
        };
        let service = Service {
            requests,
            done,
            done_tx,
            shared: pictures.shared.clone(),
            transitions: pictures.transitions.clone(),
            applied_transitions: 0,
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
        };
        (pictures, requests)
    }

    pub fn ask(&self, ask: Ask) { self.commands.send(Command::Ask(ask)) }

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

    pub fn warm_visible(&self) { self.commands.send(Command::WarmVisible) }

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

    /// The picture held of `window`, worth drawing or not.
    pub fn get(&self, window: WindowId) -> Option<Picture> { self.lock().cache.get(window).cloned() }

    /// Every window with a picture held or one on its way.
    pub fn windows(&self) -> Vec<WindowId> {
        let shared = self.lock();
        let mut windows: Vec<WindowId> = shared.cache.iter().map(|(window, _)| *window).collect();
        windows.extend(shared.schedule.owed());
        windows
    }

    /// Whether a warm of `target` would capture it now; see [`wants_warm`].
    pub fn wants_warm(&self, target: &PictureTarget) -> bool {
        wants_warm(held(&self.lock().cache, target.window), target.size, Instant::now())
    }

    /// A stand-in for a window of `size` at `scale`, never cached. `None` only if its pixels cannot be
    /// drawn.
    pub fn stand_in(&self, size: CGSize, scale: f64) -> Option<Picture> {
        let mut shared = self.lock();
        if shared.stand_in.as_ref().is_none_or(|drawn| drawn.scale != scale) {
            let (image, _) = stand_in_image(scale)?;
            shared.stand_in = Some(StandIn { scale, image });
        }
        Some(stand_in_picture(size, shared.stand_in.as_ref()?.image.clone()))
    }

    /// Hands the cache a picture taken outside the service. True when its pixels are now the ones held.
    pub fn insert(&self, window: WindowId, picture: Picture) -> bool {
        self.lock().cache.insert(window, picture)
    }

    /// Puts a freshly harvested hairline on the picture held of `window`.
    pub fn set_dressing(&self, window: WindowId, dressing: EdgeDressing) {
        if let Some(picture) = self.lock().cache.get_mut(window) {
            picture.dressing = Some(dressing);
        }
    }

    /// The desktop and the bar to dress a transition on `display` with: what the service holds of that
    /// display, and, until its first render lands, a desktop composited now on the caller's thread,
    /// which without one draws the display left behind, or black. The bar is read for presence on
    /// every transition, so a bar that has gone is not drawn, and composited once per display.
    /// `backdrop_drawn` says whether a desktop of `display` is already drawn, which a desktop without
    /// its wallpaper must not replace.
    pub fn dressing(&self, display: Display, picture_bar: bool, backdrop_drawn: bool) -> Dressing {
        let strip = picture_bar.then(|| bar_strip(display.frame));
        let bar_on_screen = strip.as_ref().is_some_and(|strip| strip.bounds.is_some());
        let (desktop, bar) = {
            let shared = self.lock();
            shared.display.for_display(shared.schedule.display(), display, bar_on_screen)
        };
        let desktop = desktop.or_else(|| composite_desktop(display, backdrop_drawn));
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

    fn keep_bar(&self, display: Display, picture: Picture, strip: CGRect) {
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
/// landed. See "Which route serves, and why the render wins" in
/// `src/animation/docs/capture-overlay-research.md`.
fn composite_desktop(display: Display, drawn: bool) -> Option<Picture> {
    let size = (display.frame.size.width, display.frame.size.height);
    let desktop = desktop_backdrop_windows(display.frame);
    let usable = capture_list_via_skylight(&desktop.windows, size, display.scale).filter(|picture| {
        is_backdrop_worth_drawing(drawn, desktop.has_wallpaper(), picture.coverage.covered, size)
    });
    if usable.is_none() {
        debug!(
            windows = desktop.windows.len(),
            has_wallpaper = desktop.has_wallpaper(),
            wanted = format!("{:.0}x{:.0}", size.0, size.1),
            "no drawable desktop yet; keeping whatever the backdrop already holds"
        );
    }
    usable
}

/// The bar listed in `strip`, captured on its own to keep its alpha, and the strip it covers. See "The
/// bar has to be captured on its own" in `src/animation/docs/capture-overlay-research.md`.
fn bar_of(strip: &BarStrip, scale: f64) -> (Option<Picture>, Option<CGRect>) {
    let Some(bounds) = strip.bounds else {
        return (None, None);
    };
    let picture = capture_list_via_skylight(
        &strip.windows,
        (bounds.size.width, bounds.size.height),
        scale,
    )
    .filter(|picture| picture.fits(bounds.size));
    (picture, Some(bounds))
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
    sinks: Vec<Sink>,
}

impl Service {
    async fn run(mut self) {
        loop {
            let wait = self.make_calls();
            tokio::select! {
                command = self.requests.recv() => match command {
                    Some((span, command)) => {
                        let _guard = span.enter();
                        self.apply(command);
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
            Command::Hold(hold) => {
                if hold == Hold::Transition {
                    self.applied_transitions += 1;
                }
                shared.schedule.hold(hold, now);
                if hold == Hold::Released {
                    drop(shared);
                    self.keep_copies();
                }
            }
            Command::Forget(window) => {
                shared.cache.forget(window);
                shared.schedule.forget(window);
            }
            Command::SetDisplay { display, picture_bar } => {
                let Shared { schedule, display: held, .. } = &mut *shared;
                if set_display(schedule, held, display, picture_bar, now) {
                    drop(shared);
                    self.warm_visible(display);
                }
            }
            Command::WarmVisible => {
                let display = shared.schedule.display();
                drop(shared);
                match display {
                    Some(display) => self.warm_visible(display),
                    None => warn!("no display geometry yet; cannot warm the snapshot cache"),
                }
            }
        }
    }

    /// Every region the cache holds, replaced by a copy of its own: a region keeps its whole composite
    /// alive. Copied off the lock, and put back only over the picture it was made of. Why a transition
    /// draws regions at all: "A region is drawn in a transition" in `src/pictures/docs/README.md`.
    fn keep_copies(&self) {
        let regions: Vec<(WindowId, Picture)> = {
            let shared = self.shared.lock().unwrap();
            shared
                .cache
                .iter()
                .filter(|(_, picture)| matches!(picture.image, Pixels::Region { .. }))
                .map(|(window, picture)| (*window, picture.clone()))
                .collect()
        };
        for (window, region) in regions {
            let Some(copy) = region.kept() else { continue };
            let taken = region.taken;
            self.shared.lock().unwrap().cache.replace_if(window, |held| held.taken == taken, copy);
        }
    }

    /// Every window on screen on `display`, pictured in the background under the ids a window rini
    /// does not manage is pictured under.
    fn warm_visible(&self, display: Display) {
        let targets: Vec<PictureTarget> =
            crate::windows::platform::window_server::visible_windows_on_display(display.frame)
                .into_iter()
                .map(|(server_id, frame)| PictureTarget {
                    window: synthetic_window_id(server_id),
                    server_id,
                    size: frame.size,
                })
                .collect();
        if targets.is_empty() {
            return;
        }
        debug!(count = targets.len(), "warming the snapshot cache");
        self.shared.lock().unwrap().schedule.ask(
            Ask {
                priority: Priority::Background,
                targets,
            },
            Instant::now(),
        );
    }

    /// Makes every call the schedule has ready; how long nothing is due for, if anything is.
    fn make_calls(&mut self) -> Option<Duration> {
        loop {
            let now = Instant::now();
            let next = {
                let mut shared = self.shared.lock().unwrap();
                let Shared { cache, schedule, .. } = &mut *shared;
                schedule.next(now, |window| held(cache, window))
            };
            let done = self.done_tx.clone();
            match next {
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
                Next::WaitUntil(at) => return Some(at.saturating_duration_since(now)),
                Next::Idle => return None,
            }
        }
    }

    /// SkyLight reads the framebuffer, so a bar read while a transition was on its way, or began, may
    /// have the overlay in it, and is not kept: that transition's lift asks again.
    fn picture_bar(&self, display: Display) {
        let before = self.transitions.load(Ordering::Acquire);
        if before != self.applied_transitions {
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
                let event = {
                    let mut shared = self.shared.lock().unwrap();
                    let kept = shared.schedule.captured(window);
                    if let Some(picture) = picture.as_ref().filter(|_| kept) {
                        debug!(
                            pid = window.pid,
                            idx = window.idx.get(),
                            covered = format!(
                                "{:.0}x{:.0}",
                                picture.coverage.covered.0, picture.coverage.covered.1
                            ),
                            window_size = format!(
                                "{:.0}x{:.0}",
                                picture.coverage.window.0, picture.coverage.window.1
                            ),
                            usable = picture.is_usable(),
                            "background snapshot landed"
                        );
                    }
                    landing(&mut shared.cache, window, kept, picture)
                };
                if let Some(event) = event {
                    for sink in &self.sinks {
                        sink(event.clone());
                    }
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
        }
    }
}

#[cfg(test)]
mod tests {
    use objc2_core_foundation::CGPoint;

    use super::*;
    use crate::pictures::platform::picture::test_picture;

    fn wid(idx: u32) -> WindowId { WindowId::new(1, idx) }

    fn sized(width: f64, height: f64) -> Picture { test_picture(CGSize::new(width, height)) }

    fn sliver() -> Picture {
        let mut picture = sized(859.0, 1081.0);
        picture.coverage.covered = (40.0, 1081.0);
        picture
    }

    fn display(id: u32, width: f64, height: f64) -> Display {
        Display {
            id,
            frame: CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(width, height)),
            scale: 2.0,
        }
    }

    fn built_in() -> Display { display(1, 1728.0, 1117.0) }

    fn external() -> Display { display(2, 3008.0, 1692.0) }

    fn strip() -> CGRect { CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(1728.0, 32.0)) }

    /// What every client hears of a background capture: the switcher redraws on `Taken`, the engine
    /// harvests on `Refused`, and a closed window's capture is never put back in the cache.
    #[test]
    fn a_capture_the_cache_took_is_taken_and_one_it_refused_for_a_blur_is_refused() {
        let mut cache = PictureCache::new();
        assert!(landing(&mut cache, wid(1), false, Some(sized(859.0, 1081.0))).is_none());
        assert!(cache.get(wid(1)).is_none(), "forgotten meanwhile: not cached");
        assert!(landing(&mut cache, wid(1), true, None).is_none(), "no picture");

        assert!(matches!(
            landing(&mut cache, wid(1), true, Some(sized(859.0, 1081.0))),
            Some(Event::Taken { window, .. }) if window == wid(1)
        ));

        let mut blurred = sized(859.0, 1081.0);
        blurred.carries_blur = true;
        assert!(matches!(landing(&mut cache, wid(2), true, Some(blurred)), Some(Event::Taken { .. })));
        assert!(matches!(
            landing(&mut cache, wid(2), true, Some(sized(859.0, 1081.0))),
            Some(Event::Refused { window }) if window == wid(2)
        ));
        assert!(
            landing(&mut cache, wid(2), true, Some(sliver())).is_none(),
            "a sliver refused is no news"
        );
    }

    /// A display change leaves nothing of the display left behind, and a bar no longer pictured goes
    /// on its own.
    #[test]
    fn a_display_change_empties_what_is_held_and_a_bar_turned_off_drops_only_the_bar() {
        let now = Instant::now();
        let mut schedule = Schedule::<()>::default();
        let mut held = DisplayPictures::default();
        assert!(set_display(&mut schedule, &mut held, built_in(), true, now));
        held.desktop = Some(sized(1728.0, 1117.0));
        held.bar = Some((sized(1728.0, 32.0), strip()));

        assert!(!set_display(&mut schedule, &mut held, built_in(), false, now));
        assert!(held.desktop.is_some(), "the desktop stays");
        assert!(held.bar.is_none(), "the bar goes");

        held.bar = Some((sized(1728.0, 32.0), strip()));
        assert!(set_display(&mut schedule, &mut held, external(), true, now));
        assert!(held.desktop.is_none() && held.bar.is_none());
    }

    /// The reported case: a render of the external display, landing after the overlay moved to the
    /// built-in one, was drawn at its own size and showed its top-left corner. A render is held only
    /// at the size of the display it was asked of, and only while that display is held.
    #[test]
    fn a_render_is_held_only_for_the_display_held_and_only_at_its_size() {
        let now = Instant::now();
        let mut schedule = Schedule::<()>::default();
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

    /// The thread's side of a service with no thread, ScreenCaptureKit or sink behind it.
    fn detached_service() -> (Service, Arc<Mutex<Shared>>) {
        let (_commands, requests) = channel::channel();
        let (done_tx, done) = channel::channel();
        let shared = Arc::new(Mutex::new(Shared::default()));
        let service = Service {
            requests,
            done,
            done_tx,
            shared: shared.clone(),
            transitions: Arc::default(),
            applied_transitions: 0,
            sinks: Vec::new(),
        };
        (service, shared)
    }

    /// No region stays in the cache past a release: each is replaced by a copy of its own, stamped as
    /// it was, and a picture that is not a region is left as it is.
    #[test]
    fn a_release_keeps_every_region_as_a_copy() {
        let (mut service, shared) = detached_service();
        let region = crate::pictures::platform::picture::test_region();
        let taken = region.taken;
        shared.lock().unwrap().cache.insert(wid(1), region);
        shared.lock().unwrap().cache.insert(wid(2), sized(859.0, 1081.0));
        service.apply(Command::Hold(Hold::Transition));
        assert!(matches!(
            shared.lock().unwrap().cache.get(wid(1)).unwrap().image,
            Pixels::Region { .. }
        ), "held: still a region");
        service.apply(Command::Hold(Hold::Released));
        let shared = shared.lock().unwrap();
        let kept = shared.cache.get(wid(1)).unwrap();
        assert!(matches!(kept.image, Pixels::Bitmap(_)));
        assert_eq!(kept.taken, taken);
        assert!(matches!(shared.cache.get(wid(2)).unwrap().image, Pixels::Bitmap(_)));
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

    /// Every window with a picture held or one on its way, but not one forgotten while its capture was
    /// out.
    #[test]
    fn windows_are_those_held_and_those_owed() {
        let (pictures, _commands) = Pictures::detached();
        let now = Instant::now();
        let target = |idx| PictureTarget {
            window: wid(idx),
            server_id: rini_core::ids::WindowServerId::new(idx),
            size: CGSize::new(859.0, 1081.0),
        };
        {
            let mut shared = pictures.lock();
            shared.cache.insert(wid(1), sized(859.0, 1081.0));
            shared.schedule.ask(
                Ask {
                    priority: Priority::Focus,
                    targets: vec![target(2), target(3)],
                },
                now,
            );
            let _ = shared.schedule.next(now, |_| None);
            shared.schedule.forget(wid(3));
        }
        let mut windows = pictures.windows();
        windows.sort();
        assert_eq!(windows, vec![wid(1), wid(2)]);
    }
}
