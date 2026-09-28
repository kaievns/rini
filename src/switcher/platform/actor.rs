//! The main-thread actor that owns the popup.
//!
//! The reactor decides WHAT to show — it holds the candidate list and the cursor — and runs on its own
//! thread. AppKit and Core Animation are main-thread only, so the panel cannot live there. This is the
//! seam: a channel the reactor writes and the main thread reads, carrying rows and a selection.
//!
//! Nothing here decides which window a switch lands on. Every message is "draw this" or "go away", so a
//! slow main thread delays the popup and nothing else — the switch itself is already correct on the
//! other side. What this does decide is WHEN the popup appears: only once a switch has been held
//! (`domain::reveal`), so a quick combo never draws.

use std::time::Instant;

use objc2::MainThreadMarker;
use tracing::{debug, warn};

use objc2_core_foundation::CGRect;

use rini_runloop::channel;

use crate::switcher::domain::reveal::{Draw, Reveal};
use crate::switcher::platform::panel::{OnPick, Row, SwitcherPanel};

pub type Sender = channel::Sender<Event>;
pub type Receiver = channel::Receiver<Event>;

/// What the main thread is asked to draw.
pub enum Event {
    /// Draw the strip with `selected` highlighted, centred on `screen` in CoreGraphics coordinates.
    ///
    /// Sent on every step as well as on the open: the panel decides for itself whether that means
    /// creating layers or just moving the highlight.
    Show {
        rows: Vec<Row>,
        selected: usize,
        screen: CGRect,
    },
    /// Take the panel off screen.
    Hide,
    /// Pictures the animation engine already held. Drawn on the next `Show`, and kept for later ones.
    Pictures(
        Vec<(
            rini_core::ids::WindowId,
            crate::animation::platform::window_snapshot::WindowSnapshot,
        )>,
    ),
}

/// Owns the panel and pumps the channel.
pub struct SwitcherActor {
    requests: Receiver,
    panel: Option<SwitcherPanel>,
    mtm: MainThreadMarker,
    /// Whether the one failure to build a panel has already been logged. Without this a machine that
    /// cannot create the window would log on every keypress.
    warned: bool,
    reveal: Reveal,
    /// Where a click on a row goes. Installed by the app, which owns the reactor this feature may not
    /// name.
    on_pick: OnPick,
    /// The latest draw asked for while the hold is being waited out, drawn when it is up.
    waiting: Option<(Vec<Row>, usize, CGRect)>,
}

impl SwitcherActor {
    pub fn new(requests: Receiver, mtm: MainThreadMarker, on_pick: OnPick) -> Self {
        Self {
            requests,
            panel: None,
            mtm,
            warned: false,
            reveal: Reveal::default(),
            on_pick,
            waiting: None,
        }
    }

    pub async fn run(mut self) {
        loop {
            let wait = self.reveal.due().map(|due| due.saturating_duration_since(Instant::now()));
            let event = tokio::select! {
                request = self.requests.recv() => match request {
                    Some((_span, event)) => event,
                    None => break,
                },
                _ = rini_runloop::executor::sleep(wait.unwrap_or_default()), if wait.is_some() => {
                    if self.reveal.on_tick(Instant::now())
                        && let Some((rows, selected, screen)) = self.waiting.take()
                    {
                        self.draw(rows, selected, screen);
                    }
                    continue;
                }
            };
            match event {
                Event::Show { rows, selected, screen } => self.show(rows, selected, screen),
                Event::Hide => {
                    self.waiting = None;
                    if self.reveal.on_hide()
                        && let Some(panel) = self.panel.as_mut()
                    {
                        panel.hide();
                    }
                }
                Event::Pictures(pictures) => {
                    // Only if a panel exists. Pictures arriving before the first switch have nowhere
                    // to go, and building a window to hold them would pay 112ms for nothing.
                    if let Some(panel) = self.panel.as_mut() {
                        let count = pictures.len();
                        panel.set_pictures(pictures);
                        if count > 0 {
                            panel.redraw();
                        }
                    }
                }
            }
        }
    }

    fn show(&mut self, rows: Vec<Row>, selected: usize, screen: CGRect) {
        self.ensure_panel();
        match self.reveal.on_show(Instant::now()) {
            Draw::Now => {
                self.waiting = None;
                self.draw(rows, selected, screen);
            }
            Draw::Later { .. } => self.waiting = Some((rows, selected, screen)),
        }
    }

    fn draw(&mut self, rows: Vec<Row>, selected: usize, screen: CGRect) {
        if let Some(panel) = self.panel.as_mut() {
            panel.show(&rows, selected, screen);
        }
    }

    /// Built on the first switch rather than at startup: creating the window costs about 112ms, and a
    /// session that never opens a switch should not pay it. Built while the hold is being waited out,
    /// so the first popup does not pay it either.
    fn ensure_panel(&mut self) {
        if self.panel.is_none() {
            self.panel = SwitcherPanel::new(self.mtm, self.on_pick.clone());
            if self.panel.is_none() {
                if !self.warned {
                    warn!("could not create the switcher panel; switching will work without it");
                    self.warned = true;
                }
                return;
            }
            debug!("switcher panel created");
        }
    }
}
