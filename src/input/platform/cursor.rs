//! The pointer as rini drives it: warp.

use objc2_core_foundation::CGPoint;
use objc2_core_graphics::{
    CGError, CGEventSource, CGEventSourceStateID, CGWarpMouseCursorPosition,
};

use crate::input::platform::cg_ok;

pub fn warp_mouse(point: CGPoint) -> Result<(), CGError> {
    let src = CGEventSource::new(CGEventSourceStateID::CombinedSessionState);
    if let Some(src) = &src {
        CGEventSource::set_local_events_suppression_interval(Some(src), 0.0);
    }
    cg_ok(CGWarpMouseCursorPosition(point))
}
