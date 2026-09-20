//! The geometry of a flight: how tiles group, travel, ease and stack. Pure functions over
//! CoreGraphics rects; nothing here draws. See `docs/animation-smoothness.md`.
pub mod easing;
pub mod fit;
pub mod frame_writes;
pub mod plan;
pub mod strip_stack;
pub mod surface;
pub mod tile;
pub mod travel;
pub mod z_group;
