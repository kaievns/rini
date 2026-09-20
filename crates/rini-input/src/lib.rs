#![allow(non_upper_case_globals)]
//! The input context: what the user asked for. Keys, bindings, trackpad gestures and drags come in
//! through CGEventTaps; commands in the `rini_ipc::protocol` language go out, so the application cannot
//! tell a hotkey from a CLI call. See `docs/architecture.md`.

// Model
pub mod binding;
pub mod drag_swap;
pub mod key;
pub mod settings;

// Events out
pub mod event;

// Adapters
pub mod cursor;
pub mod haptics;
pub mod tap;

// Actors
pub mod gesture_tap;
pub mod input_tap;

#[inline(always)]
pub(crate) fn cg_ok(err: objc2_core_graphics::CGError) -> Result<(), objc2_core_graphics::CGError> {
    if err == objc2_core_graphics::CGError::Success { Ok(()) } else { Err(err) }
}
