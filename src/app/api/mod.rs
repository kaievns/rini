//! rini's outward surface: the Mach IPC server bound to the reactor, and the shapes a query answers
//! in. The wire language itself is `rini-ipc`, so the CLI can speak it without linking the daemon.

pub mod backend;
pub mod dto;
