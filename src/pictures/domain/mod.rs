//! What a picture is worth, decided over sizes, levels and thumbprints: whether it covers its window
//! and fits the frame it is drawn into, what the cache keeps, where a window lies in a batched
//! composite, what is known of each window, when a window has settled, when a window flies a stand-in,
//! which id a window is pictured under and which of those pictures go, and which capture the service
//! makes next, the windows on screen a transition watches and those pictured afresh at rest among
//! them. Also the blur merge and the handover check, over plain bytes.

pub mod backdrop;
pub mod cache;
pub mod coverage;
pub mod handover;
pub mod identity;
pub mod knowledge;
pub mod print;
pub mod refresh;
pub mod region;
pub mod schedule;
pub mod settle;
pub mod stand_in;
pub mod target;
pub mod translucency;
pub mod watch;
