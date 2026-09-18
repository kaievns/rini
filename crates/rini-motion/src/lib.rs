//! Animation geometry: how tiles group, travel, ease and stack. Pure functions over CoreGraphics
//! rects; nothing here draws or talks to macOS. See `docs/architecture.md`.

pub mod fit;
pub mod plan;
pub mod surface;
pub mod z_group;
