//! The pointer as rini drives it: warp, hide, show.

use objc2_core_foundation::CGPoint;
use objc2_core_graphics::{
    CGDisplayHideCursor, CGDisplayShowCursor, CGError, CGEventSourceStateID, kCGNullDirectDisplay,
};

use crate::cg_ok;
use objc2_core_foundation::{CFBoolean, CFRetained, CFString, CFType, Type, kCFBooleanTrue};
use rini_skylight_sys::{CGSSetConnectionProperty, SLSMainConnectionID};
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

/// Lets a background process hide the cursor; without it `hide_mouse` is a no-op for rini.
pub fn allow_hide_mouse() -> Result<(), CGError> {
    let cid = unsafe { SLSMainConnectionID() };
    let property = CFString::from_str("SetsCursorInBackground");
    let value = CFBoolean::retain(unsafe { kCFBooleanTrue.unwrap_unchecked() });

    cg_ok(unsafe {
        CGSSetConnectionProperty(
            cid,
            cid,
            CFRetained::<CFString>::as_ptr(&property).as_ptr(),
            CFRetained::<CFBoolean>::as_ptr(&value).as_ptr() as *mut CFType,
        )
    })
}
