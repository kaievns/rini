//! The captures, through SkyLight and ScreenCaptureKit, the service that makes them on its own thread
//! and holds the cache they land in, the picture type and its stand-in, the window-server listings of
//! the desktop and the bar, the helpers that hand a picture to a layer, and the one place a surface's
//! memory is reached.

pub mod bitmap;
pub mod capturer;
pub mod desktop;
pub mod layer;
pub mod measure;
pub mod picture;
pub mod screencapturekit;
pub mod service;
pub mod skylight;
pub mod surface;
