//! Pictures of windows, the desktop and the bar.
//!
//! `domain` decides what a picture is worth and when one may be taken. `platform` takes them, through
//! SkyLight and ScreenCaptureKit, and hands them to layers. Knows windows, sizes and window-server
//! levels, not layouts or how a flight moves. See `src/pictures/docs/README.md`.

pub mod domain;
pub mod platform;
