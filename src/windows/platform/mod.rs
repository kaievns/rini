#![allow(non_upper_case_globals)]
//! Accessibility, the window server, Carbon, and the actors that drive them.

pub mod app;
pub mod app_actor;
pub mod ax;
pub mod carbon;
pub mod lifecycle;
pub mod mouse;
pub mod process;
pub mod sub_level;
pub mod window_server;

#[inline(always)]
pub(crate) fn cg_ok(err: objc2_core_graphics::CGError) -> Result<(), objc2_core_graphics::CGError> {
    if err == objc2_core_graphics::CGError::Success { Ok(()) } else { Err(err) }
}
