//! macOS glue the application itself owns: the Accessibility permission, the launch agent, power
//! state, the raw SkyLight notification adapter, and two window-server reads no context claims.
pub mod accessibility;
pub mod power;
pub mod service;
pub mod window_notify;
pub mod window_server;

#[inline(always)]
pub fn cg_ok(err: objc2_core_graphics::CGError) -> Result<(), objc2_core_graphics::CGError> {
    if err == objc2_core_graphics::CGError::Success { Ok(()) } else { Err(err) }
}
