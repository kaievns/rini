#![allow(non_upper_case_globals)]
//! macOS platform layer. Nothing here knows about workspaces or layouts; see `docs/architecture.md`.

use objc2_core_graphics::CGError;

pub mod accessibility;
pub mod app;
pub mod axuielement;
pub mod carbon;

pub mod display_churn;
pub mod enhanced_ui;
pub mod event;
pub mod event_tap;
pub mod haptics;
pub mod hotkey;
pub mod mach;
pub mod observer;
pub mod power;
pub mod process;
pub mod screen;
pub mod service;
pub mod space_switch;
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
