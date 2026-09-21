//! rini's shared kernel.
//!
//! Two things live here, and nothing else: the identity types every feature speaks, and the file
//! locations the daemon and the CLI both have to resolve. Both are vocabulary, not behaviour — a
//! module here decides nothing about windows, displays or layouts, which is what keeps this crate
//! from becoming the hub that a `shared` crate usually turns into.
//!
//! See `docs/architecture.md`.

pub mod ids;
pub mod paths;
