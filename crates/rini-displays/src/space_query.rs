//! What the window server says about a space: its kind, and which one holds focus.
use once_cell::sync::Lazy;
use rini_skylight_sys::{CGSGetActiveSpace, SLSMainConnectionID, SLSSpaceGetType};

use rini_core::ids::SpaceId;

static G_CONNECTION: Lazy<i32> = Lazy::new(|| unsafe { SLSMainConnectionID() });

pub fn space_is_user(sid: u64) -> bool { unsafe { SLSSpaceGetType(*G_CONNECTION, sid) == 0 } }
pub fn space_is_fullscreen(sid: u64) -> bool { unsafe { SLSSpaceGetType(*G_CONNECTION, sid) == 4 } }
/// The space on the display currently holding WindowServer focus.
pub fn active_space() -> SpaceId {
    SpaceId::new(unsafe { CGSGetActiveSpace(*G_CONNECTION) })
}
