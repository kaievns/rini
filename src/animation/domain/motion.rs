//! How a transition moves: how tiles group, travel, ease, stack and bounce, and the accents riding
//! them. Pure functions over CoreGraphics rects; nothing here draws. See
//! `src/animation/docs/animation-smoothness.md`.
pub mod accents;
pub mod bounce;
pub mod easing;
pub mod glide;
pub mod nudge;
pub mod plan;
pub mod score;
pub mod surface;
#[cfg(test)]
pub(crate) mod testing;
pub mod tile;
pub mod z_group;
