//! The flight engine: the actor that runs every transition, on the `transitions` thread, playing
//! each flight on `crate::animation`'s renderer on the main thread, and the thread that writes its
//! journal.

pub mod engine;
pub mod journal;
