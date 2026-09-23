#![allow(non_upper_case_globals)]
//! CGEventTaps, Carbon hotkeys, the cursor and the haptic engine.

pub mod cursor;
pub mod gesture_tap;
pub mod haptics;
pub mod input_tap;
pub mod keyboard;
pub mod tap;

#[inline(always)]
pub(crate) fn cg_ok(err: objc2_core_graphics::CGError) -> Result<(), objc2_core_graphics::CGError> {
    if err == objc2_core_graphics::CGError::Success {
        Ok(())
    } else {
        Err(err)
    }
}
