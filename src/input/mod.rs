//! What the user asked for.
//!
//! Keys, bindings, trackpad gestures and drags come in through CGEventTaps; commands in the
//! `rini_ipc::protocol` language go out, so the application cannot tell a hotkey from a CLI call.
//! See `docs/architecture.md`.
//!
//! `key` is still on both sides of the domain/platform line: the `KeySpec` parsing is pure, the
//! keyboard-layout lookup reads Carbon.

pub mod domain;
pub mod event;
pub mod key;
pub mod platform;
pub mod settings;
