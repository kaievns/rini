//! What a picture is worth, decided over sizes, levels and thumbprints: whether it covers its window
//! and fits the frame it is drawn into, what the cache keeps, where a window lies in a batched
//! composite, when a window has settled, when a window flies a stand-in, and which capture the service
//! makes next. Also the blur merge, over plain bytes.

pub mod backdrop;
pub mod cache;
pub mod coverage;
pub mod knowledge;
pub mod print;
pub mod region;
pub mod schedule;
pub mod settle;
pub mod stand_in;
pub mod target;
pub mod translucency;
