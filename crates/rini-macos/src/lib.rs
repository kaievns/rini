#![allow(non_upper_case_globals)]
//! Transitional: the macOS adapters not yet moved into their contexts (displays, input) and the
//! Mach IPC server. See `docs/architecture.md`.

use objc2_core_graphics::CGError;

pub mod accessibility;

pub mod event;
pub mod event_tap;
pub mod haptics;
pub mod hotkey;
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
