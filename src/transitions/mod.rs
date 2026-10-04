//! When things happen in a transition: frame zero, the overlay up, the real frames written,
//! pictures swapped in flight, a press merged into the flight running, the lift and the quiet after
//! it.
//!
//! `domain` holds the decisions, pure: what a request is, how a pass is sorted and staged, when a
//! picture is swapped in, when frames go out, when the overlay lifts, the lifecycle as a state
//! machine, and its journal. `platform` is the flight engine, the actor that carries them out on the
//! main thread with `crate::animation`'s renderer and `crate::pictures`' service, and the journal's
//! writer. `settings` is whether the journal is written. Driven by the reactor. See
//! `src/transitions/docs/README.md`.

pub mod domain;
pub mod platform;
pub mod settings;
