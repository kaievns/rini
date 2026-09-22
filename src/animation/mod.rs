//! Movement on screen.
//!
//! `domain::motion` plans a flight as pure geometry and `domain::pass` sorts a layout pass into what
//! moves, what stays and what warms. `platform` captures pictures of windows, draws them as Core
//! Animation tiles, and flies the tiles while the real windows move underneath. Knows windows and
//! frames, not workspaces or strips.
//!
//! See `docs/architecture.md` and `src/animation/docs/animation-smoothness.md`.

pub mod domain;
pub mod platform;
