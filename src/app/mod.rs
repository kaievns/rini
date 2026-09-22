//! The application layer: what turns the features into rini.
//!
//! `reactor` orchestrates — it holds every feature's store, reduces the events they emit, and drives
//! them back. `config` reads `config.toml` and fills each feature's settings type. `api` is the
//! outward surface: the IPC backend and the query DTOs. `notifications` demultiplexes NSWorkspace
//! for the two features that want it, `hotkeys` lowers a binding to a command, `launch_agent` owns
//! the plist, and `startup` runs the configured startup commands.
//!
//! Nothing here is imported by a feature. See `docs/architecture.md`.

pub mod api;
pub mod boot;
pub mod channels;
pub mod config;
pub mod hotkeys;
pub mod launch_agent;
pub mod logging;
pub mod notifications;
pub mod reactor;
pub mod startup;
