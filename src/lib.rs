//! rini: a scrollable-tiling window manager for macOS.
//!
//! # How this tree is arranged
//!
//! `src/` is the application. `app/` is the application layer — the composition root, the reactor
//! that orchestrates everything, the config loader, the IPC surface and the macOS notification
//! demultiplexers. Everything beside it is a feature: a vertical slice that owns its model, its
//! macOS adapters, its settings and its tests.
//!
//! ```text
//! src/app/          the application: wiring, reactor, config, IPC surface
//! src/windows/      windows and the apps that own them
//! src/displays/     screens, native spaces, coordinates
//! src/layout/       the scrolling strip
//! src/workspaces/   virtual workspaces and their persistence
//! src/input/        keys, bindings, gestures, drags
//! src/animation/    movement on screen
//! ```
//!
//! Each feature splits the same way: `domain/` is pure — model, value objects, decisions, no macOS
//! — and `platform/` is the adapters and the actors that drive them. A feature with nothing to
//! adapt has no `platform/` (`layout`), and a feature whose model is the whole of it has no
//! `domain/`.
//!
//! # The rules, and what enforces them
//!
//! 1. A feature never names `crate::app`. The application knows the features; the features do not
//!    know the application. A feature talks upward by emitting its own `event` type, which the
//!    reactor converts.
//! 2. `domain/` never touches macOS and never reads its own `platform/`. The window server's plain
//!    value types (`SpaceId`, `WindowServerId`, `DisplayReconfigFlags`) are vocabulary, not API, and
//!    are allowed.
//! 3. Features depend on features in one direction: `windows` is the most upstream, then
//!    `displays`, then `layout`, then `workspaces`. `input` and `animation` sit beside them and
//!    reach only for ids and frames.
//! 4. Reusable libraries live in `crates/`, and none of them knows what a workspace is:
//!    `rini-core` (identity and file locations), `rini-geometry`, `rini-runloop`, `rini-ipc`,
//!    `rini-mach-sys`, `rini-skylight-sys`. `crates/rini-cli` is the client binary.
//!
//! `tests/architecture.rs` checks 1 and 2 against the tree on every `cargo test`, with the two
//! files still on the wrong side of rule 2 named in it. See `docs/architecture.md`.

#![allow(stable_features)]
#![allow(non_upper_case_globals)]

pub mod animation;
pub mod app;
pub mod displays;
pub mod input;
pub mod layout;
pub mod windows;
pub mod workspaces;
