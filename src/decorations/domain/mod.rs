//! A window's decorations as pure geometry over a tile's own points: their rects and corners
//! (`shape`), their colours and gradients (`paint`), their paint by focus and who wears the ring
//! (`style`), what the window rules say each window wears (`rules`), and how deep each sits against
//! the picture (`stack`).

pub mod paint;
pub mod rest;
pub mod rules;
pub mod shape;
pub mod stack;
pub mod style;
