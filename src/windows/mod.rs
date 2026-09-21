//! Windows and the apps that own them, as rini identifies, observes and drives them.
//!
//! Knows nothing about workspaces or layouts. `domain` is the catalogue, the focus and raise rules
//! and the frame-transaction ledger, all pure; `platform` is Accessibility, the window server,
//! Carbon and the per-app actor. See `docs/architecture.md`.

pub mod domain;
pub mod event;
pub mod platform;
