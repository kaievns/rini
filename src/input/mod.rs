//! What the user asked for.
//!
//! Keys, bindings, trackpad gestures and drags come in through CGEventTaps; commands in the
//! `rini_ipc::protocol` language go out, so the application cannot tell a hotkey from a CLI call.
//! `domain::key` holds the vocabulary and the parsing that needs no keyboard; `platform::keyboard`
//! holds the rest, because which physical key `"a"` names depends on the layout in use.
//!
//! See `docs/architecture.md`.

pub mod domain;
pub mod event;
pub mod platform;
pub mod settings;
