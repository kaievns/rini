//! Shared kernel: the types every rini crate agrees on and nothing that touches a window.
//! See `docs/architecture.md` for what belongs here and what does not.

pub mod channel;
pub mod collections;
pub mod geometry;
pub mod ids;
pub mod log;
pub mod util;

pub use rini_protocol::{Direction, ResizeOrientation};
