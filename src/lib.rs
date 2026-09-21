//! rini: a scrollable-tiling window manager for macOS.
//!
//! `app/` is the application layer: the composition root, the reactor, the config loader, the IPC
//! surface. Everything beside it is a feature, and each one splits into a pure `domain/` and a macOS
//! `platform/`.
//!
//! ```text
//! src/app/          wiring, reactor, config, IPC surface
//! src/windows/      windows and the apps that own them
//! src/displays/     screens, native spaces, coordinates
//! src/layout/       the scrolling strip
//! src/workspaces/   virtual workspaces and their persistence
//! src/input/        keys, bindings, gestures, drags
//! src/animation/    movement on screen
//! ```
//!
//! The rules this layout is held to, and what enforces each, are in
//! `docs/architecture.md`. `tests/architecture.rs` checks the two the tree can be
//! read for.

#![allow(stable_features)]
#![allow(non_upper_case_globals)]

pub mod animation;
pub mod app;
pub mod displays;
pub mod input;
pub mod layout;
pub mod windows;
pub mod workspaces;
