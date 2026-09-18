//! The capture-based animation overlay: window snapshots, a Core Animation tile layer per window,
//! and the engine that flies tiles along a `rini_motion` plan while the real windows move
//! underneath. Knows windows and frames, not workspaces or strips. See `docs/architecture.md`.
pub mod edge_dressing;
pub mod engine;
pub mod overlay;
pub mod snapshot_service;
pub mod window_snapshot;
