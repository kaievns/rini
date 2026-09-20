#![allow(non_upper_case_globals)]
//! Transitional: the launch agent, permission and power helpers, the raw SkyLight notification
//! adapter, and two window-server reads, all of which the application (`rini-wm`) owns. See
//! `docs/architecture.md`.

use objc2_core_graphics::CGError;

pub mod accessibility;

pub mod power;
pub mod service;
pub mod window_notify;
pub mod window_server;

#[inline(always)]
pub fn cg_ok(err: CGError) -> Result<(), CGError> {
    if err == CGError::Success {
        Ok(())
    } else {
        Err(err)
    }
}
