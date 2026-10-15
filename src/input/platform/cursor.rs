//! The pointer as rini drives it: warp.

use objc2_core_foundation::CGPoint;
use objc2_core_graphics::{CGError, CGEventSourceStateID};

use crate::input::platform::cg_ok;
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
