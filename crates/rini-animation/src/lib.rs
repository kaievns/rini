//! The animation context: movement on screen. `motion` plans a flight as pure geometry; the
//! snapshot modules capture pictures of windows; `overlay` draws them as Core Animation tiles;
//! `engine` flies the tiles while the real windows move underneath. Knows windows and frames, not
//! workspaces or strips. See `docs/architecture.md` and `docs/animation-smoothness.md`.

// Model
pub mod motion;
pub mod pass;

// Adapters
pub mod backdrop;
pub mod edge_dressing;
pub mod overlay;
pub mod snapshot_service;
pub mod window_snapshot;

// Actor
pub mod engine;
