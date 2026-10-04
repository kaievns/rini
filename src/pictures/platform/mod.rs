//! The captures, through SkyLight and ScreenCaptureKit, the service that makes them on its own thread
//! and holds the cache they land in, the picture type and its stand-in, the window-server listings of
//! the desktop and the bar, and the helpers that hand a picture to a layer.

pub mod bitmap;
pub mod desktop;
pub mod edge_dressing;
pub mod layer;
pub mod measure;
pub mod picture;
pub mod screencapturekit;
pub mod service;
pub mod skylight;
