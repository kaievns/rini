//! Transitional: collections, geometry, log and util. Geometry becomes `rini-geometry`; the rest
//! goes to its only users. See `docs/architecture.md`.

pub mod collections;
pub mod geometry;
pub mod log;
pub mod util;

pub use rini_ipc::protocol::{Direction, ResizeOrientation};
