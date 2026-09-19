
use objc2_core_foundation::CGPoint;
use objc2_core_graphics::{
    CGDisplayHideCursor, CGDisplayShowCursor, CGError, CGEventSourceStateID, kCGNullDirectDisplay,
};

pub use rini_windows::window_server::current_cursor_location;
use crate::cg_ok;
pub use crate::hotkey::{Hotkey, HotkeySpec, KeyCode, Modifiers};
use rini_skylight_sys::{
    CFRelease, CGEventSourceCreate, CGEventSourceSetLocalEventsSuppressionInterval,
    CGWarpMouseCursorPosition,
};









pub fn warp_mouse(point: CGPoint) -> Result<(), CGError> {
    let src = unsafe { CGEventSourceCreate(CGEventSourceStateID::CombinedSessionState) };
    unsafe { CGEventSourceSetLocalEventsSuppressionInterval(src, 0.0) };

    let res = cg_ok(unsafe { CGWarpMouseCursorPosition(point) });
    unsafe { CFRelease(src) };
    res
}

pub fn hide_mouse() -> Result<(), CGError> {
    cg_ok(CGDisplayHideCursor(kCGNullDirectDisplay))
}

pub fn show_mouse() -> Result<(), CGError> {
    cg_ok(CGDisplayShowCursor(kCGNullDirectDisplay))
}


