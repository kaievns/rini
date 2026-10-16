//! What the window server says about a space: its kind, and which one holds focus.
use rini_core::ids::SpaceId;
use rini_skylight_sys::{CGSGetActiveSpace, G_CONNECTION, SLSSpaceGetType};

/// `SLSSpaceGetType` of `sid`.
fn space_type(sid: u64) -> i32 {
    // SAFETY: integer arguments only, so no memory of rini's is at stake.
    unsafe { SLSSpaceGetType(*G_CONNECTION, sid) }
}

pub fn space_is_user(sid: u64) -> bool { space_type(sid) == 0 }
pub fn space_is_fullscreen(sid: u64) -> bool { space_type(sid) == 4 }
/// The space on the display currently holding WindowServer focus. With two displays this needs
/// checking (`specs/focus.md`, "Which window is focused").
pub fn active_space() -> SpaceId {
    // SAFETY: an integer argument only, so no memory of rini's is at stake.
    SpaceId::new(unsafe { CGSGetActiveSpace(*G_CONNECTION) })
}
