//! The main-thread actor that owns the popup.
//!
//! The reactor decides WHAT to show — it holds the candidate list and the cursor — and runs on its own
//! thread. AppKit and Core Animation are main-thread only, so the panel cannot live there. This is the
//! seam: a channel the reactor writes and the main thread reads, carrying rows and a selection.
//!
//! Nothing here decides anything. Every message is "draw this" or "go away", so a slow main thread
//! delays the popup and nothing else — the switch itself is already correct on the other side.

use objc2::MainThreadMarker;
use tracing::{debug, warn};

use objc2_core_foundation::CGRect;

use rini_runloop::channel;

use crate::switcher::platform::panel::{Row, SwitcherPanel};

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
}

/// Owns the panel and pumps the channel.
pub struct SwitcherActor {
    requests: Receiver,
    panel: Option<SwitcherPanel>,
    mtm: MainThreadMarker,
    /// Whether the one failure to build a panel has already been logged. Without this a machine that
    /// cannot create the window would log on every keypress.
    warned: bool,
}

impl SwitcherActor {
    pub fn new(requests: Receiver, mtm: MainThreadMarker) -> Self {
        Self {
            requests,
            panel: None,
            mtm,
            warned: false,
        }
    }

    pub async fn run(mut self) {
        while let Some((_span, event)) = self.requests.recv().await {
            match event {
                Event::Show { rows, selected, screen } => self.show(rows, selected, screen),
                Event::Hide => {
                    if let Some(panel) = self.panel.as_mut() {
                        panel.hide();
                    }
                }
            }
        }
    }

    fn show(&mut self, rows: Vec<Row>, selected: usize, screen: CGRect) {
        // Built on first use rather than at startup: creating the window costs about 112ms, and a
        // session that never opens a switch should not pay it.
        if self.panel.is_none() {
            self.panel = SwitcherPanel::new(self.mtm);
            if self.panel.is_none() {
                if !self.warned {
                    warn!("could not create the switcher panel; switching will work without it");
                    self.warned = true;
                }
                return;
            }
            debug!("switcher panel created");
        }
        if let Some(panel) = self.panel.as_mut() {
            panel.show(&rows, selected, screen);
        }
    }
}
