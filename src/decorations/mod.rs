//! What a window wears: the hairline macOS composites around every window, a white line on the
//! window's outermost point and a dark outline just outside it, and the shadow the window casts,
//! which no capture carries, so the overlay draws both over every tile; and niri's focus ring, which
//! rini draws itself, on the tiles and at rest. `domain` is their shape, colours, style and depth,
//! pure, in a tile's own points, and when and where the ring is drawn at rest, in the screen's;
//! `platform` the layers each tile wears them on and the windows that draw the ring at rest;
//! `settings` what the config says of the ring. Reads `windows` and `displays`.
//!
//! The hairline is measured in "The hairline is composited outside every capture" in
//! `src/pictures/docs/capture-research.md`, and drawn as "The hairline and the shadow, as drawn" in
//! `src/decorations/docs/README.md` describes.

pub mod domain;
pub mod platform;
pub mod settings;
