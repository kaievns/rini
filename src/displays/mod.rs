//! Screens, native spaces and coordinates, as rini identifies, observes and switches them.
//!
//! Tracks which windows sit on which space; knows nothing about workspaces or layouts. `domain` is
//! the topology snapshot and the space-activation policy; `platform` is CGDisplay, NSScreen and the
//! SkyLight space calls, plus the actors over them. See `docs/architecture.md`.
//!
//! `screen` is the one module still on both sides of that line: `ScreenInfo`, `CoordinateConverter`
//! and the bounds arithmetic are pure, while `Actual` reads NSScreen and CGDisplay. The `System`
//! trait it is already generic over is the seam to split it on.

pub mod domain;
pub mod event;
pub mod platform;
pub mod screen;
