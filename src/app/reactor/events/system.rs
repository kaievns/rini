use rini_core::ids::WindowId;
use tracing::debug;

use crate::app::controller::Sender as WmSender;
use crate::app::reactor::MenuState;
use crate::app::reactor::events::EventOutcome;
use crate::app::reactor::managers::CommunicationManager;
use crate::windows::domain::raise;
use crate::windows::domain::request::RaiseSequence;

pub fn handle_menu_opened(menu_state: &mut MenuState, pid: i32) -> EventOutcome {
    *menu_state = match *menu_state {
        MenuState::Closed => {
            debug!(pid, "menu opened");
            MenuState::Open(pid)
        }
        MenuState::Open(owner) if owner == pid => {
            debug!(
                pid,
                "menu already open for app; ignoring duplicate menu-open notification"
            );
            MenuState::Open(owner)
        }
        MenuState::Open(owner) => {
            debug!(
                pid,
                owner,
                "menu-open owner changed without a close notification; replacing stale state"
            );
            MenuState::Open(pid)
        }
    };
    EventOutcome::focus_changed(None, false).with_focus_follows_mouse_refresh()
}

pub fn handle_menu_closed(menu_state: &mut MenuState, pid: i32) -> EventOutcome {
    let refresh = match *menu_state {
        MenuState::Closed => {
            debug!(pid, "menu closed while no menu was marked open");
            true
        }
        MenuState::Open(owner) if owner == pid => {
            debug!(pid, "menu closed; clearing menu-open state");
            *menu_state = MenuState::Closed;
            true
        }
        MenuState::Open(owner) => {
            debug!(
                pid,
                owner, "ignoring menu-closed notification for non-owning app"
            );
            false
        }
    };
    let outcome = EventOutcome::focus_changed(None, false);
    if refresh {
        outcome.with_focus_follows_mouse_refresh()
    } else {
        outcome
    }
}

pub fn handle_system_woke() -> EventOutcome { EventOutcome::window_notification_refresh() }

#[derive(Debug, Clone, Copy)]
pub struct RaiseCompletedPayload {
    pub window: WindowId,
    pub sequence: RaiseSequence,
}

pub fn handle_raise_completed(payload: RaiseCompletedPayload) -> EventOutcome {
    let RaiseCompletedPayload {
        window: window_id,
        sequence: sequence_id,
    } = payload;
    EventOutcome::no_change()
        .with_raise_request(raise::Event::RaiseCompleted { window_id, sequence_id })
}

pub fn handle_raise_timeout(sequence_id: RaiseSequence) -> EventOutcome {
    EventOutcome::no_change().with_raise_request(raise::Event::RaiseTimeout { sequence_id })
}

pub fn handle_register_wm_sender(
    communication: &mut CommunicationManager,
    sender: WmSender,
) -> EventOutcome {
    communication.wm_sender = Some(sender);
    EventOutcome::no_change()
}
