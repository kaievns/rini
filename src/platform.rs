//! macOS glue the application itself owns: the launch agent.
pub mod service;

#[inline(always)]
pub fn cg_ok(err: objc2_core_graphics::CGError) -> Result<(), objc2_core_graphics::CGError> {
    if err == objc2_core_graphics::CGError::Success { Ok(()) } else { Err(err) }
}
