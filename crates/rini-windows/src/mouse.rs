//! The pointer state window events are stamped with (the event tap sets it; the app actor reads
//! it, so a frame change during a drag is not mistaken for a layout echo), and the synthetic
//! Cmd-W that closes a window.
use std::sync::atomic::{AtomicU8, Ordering};

use objc2_core_graphics::{CGEvent, CGEventField, CGEventFlags};
use serde::{Deserialize, Serialize};

use rini_core::ids::pid_t;

#[derive(Serialize, Deserialize, Debug, Copy, Clone, Eq, PartialEq)]
#[repr(u8)]
pub enum MouseState {
    Up = 1,
    Down = 2,
}
const MOUSE_STATE_UNKNOWN: u8 = 0;
static MOUSE_STATE: AtomicU8 = AtomicU8::new(MOUSE_STATE_UNKNOWN);
const RINI_SYNTHETIC_EVENT_MARKER: i64 = 0x5249_4654;
const KEYCODE_W: u16 = 0x0d;
impl From<MouseState> for u8 {
    fn from(state: MouseState) -> u8 {
        state as u8
    }
}
impl TryFrom<u8> for MouseState {
    type Error = ();

    fn try_from(val: u8) -> Result<Self, Self::Error> {
        match val {
            x if x == MouseState::Up as u8 => Ok(MouseState::Up),
            x if x == MouseState::Down as u8 => Ok(MouseState::Down),
            _ => Err(()),
        }
    }
}
pub fn set_mouse_state(state: MouseState) {
    MOUSE_STATE.store(state.into(), Ordering::Relaxed);
}
pub fn get_mouse_state() -> Option<MouseState> {
    match MouseState::try_from(MOUSE_STATE.load(Ordering::Relaxed)) {
        Ok(s) => Some(s),
        Err(_) => None,
    }
}
/// Ask an application to handle its standard Command-W action.
///
/// Posting to the owning process preserves application-specific close behavior (for example,
/// closing a tab or prompting to save) instead of pressing the window's AX close button.
pub fn post_command_w(pid: pid_t) -> bool {
    let Some(key_down) = CGEvent::new_keyboard_event(None, KEYCODE_W, true) else {
        return false;
    };
    let Some(key_up) = CGEvent::new_keyboard_event(None, KEYCODE_W, false) else {
        return false;
    };

    for event in [&key_down, &key_up] {
        CGEvent::set_flags(Some(event), CGEventFlags::MaskCommand);
        CGEvent::set_integer_value_field(
            Some(event),
            CGEventField::EventSourceUserData,
            RINI_SYNTHETIC_EVENT_MARKER,
        );
        CGEvent::post_to_pid(pid, Some(event));
    }
    true
}
pub fn is_rini_synthetic_event(event: &CGEvent) -> bool {
    CGEvent::integer_value_field(Some(event), CGEventField::EventSourceUserData)
        == RINI_SYNTHETIC_EVENT_MARKER
}
