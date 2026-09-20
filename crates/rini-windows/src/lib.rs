#![allow(non_upper_case_globals)]
//! The windows context: windows and the apps that own them, as rini identifies, observes and
//! drives them through Accessibility and the window server. Knows nothing about workspaces or
//! layouts. See `docs/architecture.md`.

// Model
pub mod catalogue;
pub mod ids;
pub mod rules;
pub mod state;
pub mod transaction;

// Events out
pub mod event;

// Adapters
pub mod app;
pub mod ax;
pub mod carbon;
pub mod mouse;
pub mod process;
pub mod sub_level;
pub mod window_server;

// Actors
pub mod app_actor;
pub mod lifecycle;

#[inline(always)]
pub(crate) fn cg_ok(err: objc2_core_graphics::CGError) -> Result<(), objc2_core_graphics::CGError> {
    if err == objc2_core_graphics::CGError::Success { Ok(()) } else { Err(err) }
}
