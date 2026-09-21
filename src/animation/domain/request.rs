//! What the flight engine and the snapshot service are asked for.
//!
//! The ports into `crate::animation::platform`: `domain::motion` and `domain::pass` plan a pass in
//! these terms, and the engine and the capture service carry them out.

use objc2_core_foundation::{CGRect, CGSize};

use rini_core::ids::{WindowId, WindowServerId};

#[derive(Debug, Clone)]
pub struct AnimationRequest {
    pub window: WindowId,
    pub server_id: WindowServerId,
    /// Frame the window is leaving, in display coordinates.
    pub from: CGRect,
    /// Frame the window is arriving at, in display coordinates.
    pub to: CGRect,
    /// Off the strip, and so in the other z-order group.
    pub floating: bool,
}

#[derive(Debug, Clone, Copy)]
pub struct SnapshotTarget {
    pub window: WindowId,
    pub server_id: WindowServerId,
    /// The window's full size in points, as the layout intends it.
    pub size: CGSize,
}
