//! How a transition moves: how tiles group, travel, ease, stack and bounce, the accents riding them,
//! and the score all of it is written in, as edits. Pure functions over CoreGraphics rects; nothing
//! here draws, and the looks that compose a score are `super::choreography`. See
//! `src/animation/docs/animation-smoothness.md`.
pub mod accents;
pub mod bounce;
pub mod easing;
pub mod glide;
pub mod hairline;
pub mod holes;
pub mod nudge;
pub mod plan;
pub mod score;
pub mod surface;
#[cfg(test)]
pub(crate) mod testing;
pub mod tile;
pub mod z_group;
