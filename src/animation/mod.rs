//! How things move on screen.
//!
//! `domain::motion` is a transition's movement as pure geometry: rigid pieces and how a pass merges
//! into them, the curves, the depth bands, and the accents riding them, the edge bounce and a move's
//! nudge. `platform` is the renderer: it draws pictures of windows, which `crate::pictures` takes, as
//! Core Animation tiles and plays the movement while the real windows move underneath. When each step
//! happens is `crate::transitions`'. Knows windows, frames and the strip as one surface that moves;
//! never names `crate::layout` or `crate::workspaces`.
//!
//! See `docs/architecture.md` and `src/animation/docs/animation-smoothness.md`.

pub mod domain;
pub mod platform;
