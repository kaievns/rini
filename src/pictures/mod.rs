//! Pictures of windows, the desktop and the bar.
//!
//! `domain` decides what a picture is worth, and which capture the service makes next. `platform`
//! takes them, through SkyLight and ScreenCaptureKit, on the service's own thread, and hands them to
//! layers. `event` is what the service tells its clients. Knows windows, sizes, window-server levels
//! and `decorations`' fallback radius, which the stand-in's corners are drawn with, not layouts or
//! how a flight moves. See `src/pictures/docs/README.md`.

pub mod domain;
pub mod event;
pub mod platform;
