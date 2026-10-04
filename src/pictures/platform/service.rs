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
use crate::pictures::platform::desktop::{bar_strip, desktop_backdrop_windows};
use crate::pictures::platform::edge_dressing::EdgeDressing;
use crate::pictures::platform::picture::{Picture, stand_in_picture, stand_in_image};
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
    /// The pictures of the display the overlay flies on, which belong to it alone.
    display: Option<DisplayPictures>,
    stand_in: Option<StandIn>,
}

struct DisplayPictures {
    display: Display,
    /// The desktop as ScreenCaptureKit rendered it: the only source that reliably has the wallpaper.
    desktop: Option<Picture>,
    /// The last usable picture of the bar, and the strip it covers.
    bar: Option<(Picture, CGRect)>,
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

    /// The desktop and the bar to dress a transition on `display` with. What the service holds of that
    /// display, or, before it holds any, the first transition after a start or a display change,
    /// composites taken now on the caller's thread: without them that transition draws the display
    /// left behind, or black. `backdrop_drawn` says whether a desktop of `display` is already drawn,
    /// which a desktop without its wallpaper must not replace.
    pub fn dressing(&self, display: Display, picture_bar: bool, backdrop_drawn: bool) -> Dressing {
        let (desktop, bar) = {
            let shared = self.lock();
            let held = shared.display.as_ref().filter(|held| held.display == display);
            (
                held.and_then(|held| held.desktop.clone()),
                held.and_then(|held| held.bar.clone()),
            )
        };
        let desktop = desktop.or_else(|| composite_desktop(display, backdrop_drawn));
        if !picture_bar {
            return Dressing {
                desktop,
                bar: None,
                strip: None,
            };
        }
        let (bar, strip) = match bar {
            Some((picture, strip)) => (Some(picture), Some(strip)),
            None => {
                let (picture, strip) = bar_of(display);
                if let (Some(picture), Some(strip)) = (&picture, strip) {
                    self.keep_bar(display, picture.clone(), strip);
                }
                (picture, strip)
            }
        };
        Dressing { desktop, bar, strip }
    }

    fn keep_bar(&self, display: Display, picture: Picture, strip: CGRect) {
        if let Some(held) = self.lock().display.as_mut().filter(|held| held.display == display) {
            held.bar = Some((picture, strip));
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

/// The bar of `display`, captured on its own to keep its alpha, and the strip it covers. See "The bar
/// has to be captured on its own" in `src/animation/docs/capture-overlay-research.md`.
fn bar_of(display: Display) -> (Option<Picture>, Option<CGRect>) {
    let strip = bar_strip(display.frame);
    let Some(bounds) = strip.bounds else {
        return (None, None);
    };
    let picture = capture_list_via_skylight(
        &strip.windows,
        (bounds.size.width, bounds.size.height),
        display.scale,
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
            }
            Command::Forget(window) => {
                shared.cache.forget(window);
                shared.schedule.forget(window);
            }
            Command::SetDisplay { display, picture_bar } => {
                if shared.schedule.set_display(display, picture_bar, now) {
                    shared.display = Some(DisplayPictures {
                        display,
                        desktop: None,
                        bar: None,
                    });
                    drop(shared);
                    self.warm_visible(display);
                } else if !picture_bar
                    && let Some(held) = shared.display.as_mut()
                {
                    held.bar = None;
                }
            }
            Command::WarmVisible => {
                let display = shared.display.as_ref().map(|held| held.display);
                drop(shared);
                match display {
                    Some(display) => self.warm_visible(display),
                    None => warn!("no display geometry yet; cannot warm the snapshot cache"),
                }
            }
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
        let (picture, strip) = bar_of(display);
        if self.transitions.load(Ordering::Acquire) != before {
            return;
        }
        let (Some(picture), Some(strip)) = (picture, strip) else {
            return;
        };
        let mut shared = self.shared.lock().unwrap();
        if let Some(held) = shared.display.as_mut().filter(|held| held.display == display) {
            held.bar = Some((picture, strip));
        }
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
                    picture.filter(|_| kept).and_then(|picture| {
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
                        let usable = picture.is_usable();
                        if shared.cache.insert(window, picture.clone()) {
                            Some(Event::Taken { window, picture })
                        } else {
                            usable.then_some(Event::Refused { window })
                        }
                    })
                };
                if let Some(event) = event {
                    for sink in &self.sinks {
                        sink(event.clone());
                    }
                }
            }
            Done::Desktop { display, picture } => {
                let size = (display.frame.size.width, display.frame.size.height);
                // A render of another size than the display asked of is no picture of it.
                let picture =
                    picture.filter(|picture| spans_display(picture.coverage.covered, size));
                let mut shared = self.shared.lock().unwrap();
                if shared.schedule.desktop_rendered(display, picture.as_ref().map(|p| p.taken))
                    && let Some(held) =
                        shared.display.as_mut().filter(|held| held.display == display)
                {
                    debug!(covered = format!("{:.0}x{:.0}", size.0, size.1), "desktop capture landed");
                    held.desktop = picture;
                }
            }
        }
    }
}
