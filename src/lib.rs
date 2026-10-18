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
//! src/input/        keys, bindings, drags
//! src/decorations/  what a window wears: its ring and its border
//! src/pictures/     pictures of windows, the desktop and the bar
//! src/animation/    how things move on screen
//! src/transitions/  when things happen in a transition
//! src/switcher/     the window switcher popup
//! src/overview/     every workspace of a display at once, while its key is held
//! src/bar/          the menu bar across the top of every display
//! ```
//!
//! The rules this layout is held to, and what enforces each, are in
//! `docs/architecture.md`. `tests/architecture.rs` checks the ones the tree can be
//! read for.

pub mod animation;
pub mod app;
pub mod bar;
pub mod decorations;
pub mod displays;
pub mod input;
pub mod layout;
pub mod overview;
pub mod pictures;
pub mod switcher;
pub mod transitions;
pub mod windows;
pub mod workspaces;
