//! Transitional: ids, collections and geometry that have not yet moved to their contexts.
//! Dissolves as `rini-windows` and `rini-displays` land. See `docs/architecture.md`.

pub mod collections;
pub mod geometry;
pub mod ids;
pub mod log;
pub mod util;

pub use rini_protocol::{Direction, ResizeOrientation};
