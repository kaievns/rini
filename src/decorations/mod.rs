//! What a window wears: the hairline macOS composites around every window, a white line on the
//! window's outermost point and a dark outline just outside it, and the shadow the window casts.
//! No capture carries either, so the overlay draws both over every tile. `domain` is their shape,
//! style and depth, pure, in a tile's own points. Names no feature.
//!
//! The hairline is measured in "The hairline is composited outside every capture" in
//! `src/pictures/docs/capture-research.md` and drawn as "The hairline" in
//! `src/animation/docs/animation-smoothness.md`. See `src/decorations/docs/README.md`.

pub mod domain;
