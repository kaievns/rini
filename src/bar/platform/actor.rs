//! The main-thread actor that owns every display's bar.
//!
//! It is told what to show and never asks: a model from the reactor, pictures from the menu-extras
//! thread, and the clock it reads itself at each minute. Between those it sleeps, with nothing but
//! the minute's timer running.

use std::collections::hash_map::Entry;
use std::rc::Rc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use objc2::MainThreadMarker;
use objc2_core_graphics::CGDisplayBounds;
use objc2_quartz_core::CATransaction;
use rini_runloop::channel;
use rini_runloop::executor::sleep;
use rustc_hash::FxHashMap as HashMap;
use tracing::{debug, warn};

use crate::bar::domain::format::until_next_minute;
use crate::bar::domain::layout::{self, Target};
use crate::bar::domain::model::{Action, BarModel};
use crate::bar::domain::motion::{Fade, Folding, fade_length};
use crate::bar::domain::pieces::{self, Click, Clock, Context};
use crate::bar::platform::menu_extras::{Extras, Watcher};
use crate::bar::platform::panel::{BarPanel, OnClick, SCALE};
use crate::bar::platform::text::Text;

pub type Sender = channel::Sender<Event>;
pub type Receiver = channel::Receiver<Event>;

/// Where a click goes. Installed by the app, which owns the reactor this feature may not name.
pub type OnAction = Rc<dyn Fn(Action)>;

pub enum Event {
    /// What every display's bar shows. Sent only when it changed; no displays takes the bars down.
    Model(BarModel),
    /// The menu extras as last pictured, sent by the watcher thread when a picture changed.
    Extras(Extras),
    /// A flight started (`true`), or the last one has settled (`false`).
    Flight(bool),
    /// The machine woke or the clock was changed, so the time is read again now.
    ClockChanged,
}

pub struct BarActor {
    requests: Receiver,
    sender: Sender,
    mtm: MainThreadMarker,
    on_action: OnAction,
    /// Clicks from the bars' views, by display.
    clicks: channel::Sender<(String, Target)>,
    clicked: channel::Receiver<(String, Target)>,
    model: BarModel,
    extras: Extras,
    clock: Clock,
    flight: bool,
    folding: Folding,
    /// When the tail's fade out has run and the glyphs fold.
    refold_at: Option<Instant>,
    tray_open: bool,
    /// By display uuid. Made when a model first names the display, and kept.
    panels: HashMap<String, BarPanel>,
    text: Text,
    watcher: Option<Watcher>,
    paused: bool,
    /// Whether the one failure to make a panel has been logged.
    warned: bool,
}

impl BarActor {
    pub fn new(
        requests: Receiver,
        sender: Sender,
        mtm: MainThreadMarker,
        on_action: OnAction,
    ) -> Self {
        let (clicks, clicked) = channel::channel();
        Self {
            requests,
            sender,
            mtm,
            on_action,
            clicks,
            clicked,
            model: BarModel::default(),
            extras: Extras::default(),
            clock: read_clock().0,
            flight: false,
            folding: Folding::default(),
            refold_at: None,
            tray_open: true,
            panels: HashMap::default(),
            text: Text::new(SCALE),
            watcher: None,
            paused: false,
            warned: false,
        }
    }

    pub async fn run(mut self) {
        loop {
            let minute =
                (!self.model.displays.is_empty()).then(|| until_next_minute(read_clock().1));
            let refold = self.refold_at.map(|at| at.saturating_duration_since(Instant::now()));
            let wake = tokio::select! {
                request = self.requests.recv() => match request {
                    Some((_span, event)) => Wake::Event(event),
                    None => break,
                },
                Some((_span, (display, target))) = self.clicked.recv() => {
                    Wake::Click(display, target)
                }
                _ = sleep(minute.unwrap_or_default()), if minute.is_some() => Wake::Minute,
                _ = sleep(refold.unwrap_or_default()), if refold.is_some() => Wake::Refold,
            };
            match wake {
                Wake::Event(event) => self.handle(event),
                Wake::Click(display, target) => self.click(&display, target),
                Wake::Minute => self.tick(),
                Wake::Refold => {
                    self.refold_at = None;
                    self.folding = self.folding.faded();
                    self.redraw();
                }
            }
        }
    }

    fn handle(&mut self, event: Event) {
        match event {
            Event::Model(model) => {
                self.model = model;
                self.start_watcher();
                self.pause_watcher();
                self.redraw();
            }
            Event::Extras(extras) => {
                self.extras = extras;
                self.redraw();
            }
            Event::Flight(flight) => {
                self.flight = flight;
                self.pause_watcher();
            }
            Event::ClockChanged => {
                self.clock = read_clock().0;
                if let Some(watcher) = self.watcher.as_ref().filter(|_| !self.paused) {
                    watcher.refresh();
                }
                self.redraw();
            }
        }
    }

    fn tick(&mut self) {
        let clock = read_clock().0;
        if clock != self.clock {
            self.clock = clock;
            self.redraw();
        }
    }

    fn click(&mut self, display: &str, target: Target) {
        match pieces::click(target, display) {
            Click::Ask(action) => (self.on_action)(action),
            Click::Fold => {
                let (folding, fade) = self.folding.clicked();
                self.folding = folding;
                let now = objc2_quartz_core::CACurrentMediaTime();
                match fade {
                    Fade::In => {
                        self.refold_at = None;
                        cut(|| {
                            self.draw_all();
                            for panel in self.panels.values().filter(|panel| panel.visible()) {
                                panel.fade(Fade::In, now);
                            }
                        });
                    }
                    Fade::Out => {
                        let longest = cut(|| {
                            self.panels
                                .values()
                                .filter(|panel| panel.visible())
                                .map(|panel| panel.fade(Fade::Out, now))
                                .max()
                                .unwrap_or(0)
                        });
                        self.refold_at = Some(Instant::now() + fade_length(longest));
                    }
                }
            }
            Click::Tray => {
                self.tray_open = !self.tray_open;
                cut(|| {
                    self.draw_all();
                    for panel in self.panels.values().filter(|panel| panel.visible()) {
                        panel.slide(self.tray_open);
                    }
                });
            }
        }
    }

    fn redraw(&mut self) {
        cut(|| self.draw_all());
    }

    /// Draws every display's bar and orders out the rest. Inside a transaction the caller holds.
    fn draw_all(&mut self) {
        let mut drawn: Vec<&str> = Vec::new();
        for bar in &self.model.displays {
            let display = CGDisplayBounds(bar.screen);
            if display.size.width <= 0.0 {
                continue;
            }
            let panel = match self.panels.entry(bar.uuid.clone()) {
                Entry::Occupied(made) => made.into_mut(),
                Entry::Vacant(slot) => {
                    match BarPanel::new(self.mtm, on_click(&self.clicks, &bar.uuid)) {
                        Some(panel) => {
                            debug!(display = %bar.uuid, "bar panel created");
                            slot.insert(panel)
                        }
                        None => {
                            if !self.warned {
                                warn!("could not create a bar panel; the bar is not drawn");
                                self.warned = true;
                            }
                            continue;
                        }
                    }
                }
            };
            if !panel.place(display) {
                continue;
            }
            let fold = layout::fold(bar.glyphs.len(), self.folding.expanded());
            let context = Context {
                fold: &fold,
                clock: self.clock,
                tray_open: self.tray_open,
            };
            panel.draw(bar, context, &self.extras, &mut self.text);
            panel.show();
            drawn.push(&bar.uuid);
        }
        for (uuid, panel) in self.panels.iter_mut() {
            if !drawn.contains(&uuid.as_str()) {
                panel.hide();
            }
        }
        self.text.sweep();
    }

    /// Started with the first model that has a display, so with the bar off it never runs.
    fn start_watcher(&mut self) {
        if self.watcher.is_some() || self.model.displays.is_empty() {
            return;
        }
        let sender = self.sender.clone();
        self.watcher = Some(Watcher::spawn(Box::new(move |extras| {
            sender.send(Event::Extras(extras))
        })));
        self.paused = false;
    }

    fn pause_watcher(&mut self) {
        let Some(watcher) = &self.watcher else {
            return;
        };
        let paused = pictures_paused(self.flight, self.model.displays.len());
        if paused != self.paused {
            watcher.set_paused(paused);
            self.paused = paused;
        }
    }
}

/// Sends a click on `display`'s bar back to the actor, which owns what it changes.
fn on_click(clicks: &channel::Sender<(String, Target)>, display: &str) -> OnClick {
    let (clicks, display) = (clicks.clone(), display.to_string());
    Rc::new(move |target| clicks.send((display.clone(), target)))
}

enum Wake {
    Event(Event),
    Click(String, Target),
    Minute,
    Refold,
}

/// One transaction for the whole change, with no implicit animation: everything that moves is
/// animated explicitly, and everything else is a cut.
fn cut<R>(change: impl FnOnce() -> R) -> R {
    CATransaction::begin();
    CATransaction::setDisableActions(true);
    let out = change();
    CATransaction::commit();
    out
}

/// No pictures while a flight runs or settles, and none while no bar is up to show them.
fn pictures_paused(flight: bool, displays: usize) -> bool {
    flight || displays == 0
}

/// The local time, and how far into its minute it is.
fn read_clock() -> (Clock, f64) {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or(Duration::ZERO);
    let seconds = now.as_secs() as libc::time_t;
    // SAFETY: `tm` is plain data, and both pointers are to live locals.
    let tm = unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&seconds, &mut tm);
        tm
    };
    let clock = Clock {
        weekday: tm.tm_wday as u32,
        day: tm.tm_mday as u32,
        hour: tm.tm_hour as u32,
        minute: tm.tm_min as u32,
    };
    (clock, tm.tm_sec as f64 + now.subsec_nanos() as f64 / 1e9)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_flight_pauses_the_pictures() {
        assert!(pictures_paused(true, 2));
        assert!(!pictures_paused(false, 2));
    }

    /// With no bar up there is nothing to picture for.
    #[test]
    fn no_bars_pause_the_pictures() {
        assert!(pictures_paused(false, 0));
    }
}
