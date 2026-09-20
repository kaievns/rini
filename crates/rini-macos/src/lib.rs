#![allow(non_upper_case_globals)]
//! Transitional: the Mach IPC server (to `rini-ipc`), the launch agent and permission helpers (to
//! `rini-wm`), the raw SkyLight notification adapter, and a few window-server reads waiting for
//! `rini-animation`. See `docs/architecture.md`.

use objc2_core_graphics::CGError;

pub mod accessibility;

pub mod mach;
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
