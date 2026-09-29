//! The main-thread actor that owns every display's bar.

use std::rc::Rc;

use objc2::MainThreadMarker;
use rini_runloop::channel;

use crate::bar::domain::model::{Action, BarModel};
use crate::bar::platform::menu_extras::Extras;

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
}

impl BarActor {
    pub fn new(requests: Receiver, sender: Sender, mtm: MainThreadMarker, on_action: OnAction) -> Self {
        Self { requests, sender, mtm, on_action }
    }

    pub async fn run(mut self) {
        let _ = (&self.sender, self.mtm, &self.on_action);
        while let Some((_span, event)) = self.requests.recv().await {
            drop(event);
        }
    }
}
