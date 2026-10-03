//! What a picture is worth, decided over sizes, levels and thumbprints: whether it covers its window
//! and fits the frame it is drawn into, what the cache keeps, when a window has settled, when a window
//! flies a stand-in, and which capture work a flight permits. Also the blur merge, over plain bytes.

pub mod backdrop;
pub mod cache;
pub mod coverage;
pub mod schedule;
pub mod settle;
pub mod stand_in;
pub mod target;
pub mod translucency;
