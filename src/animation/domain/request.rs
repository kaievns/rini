//! What the flight engine is asked for.
//!
//! The port into `crate::animation::platform`: `domain::motion` and `domain::pass` plan a pass in
//! these terms, and the engine carries them out.

use objc2_core_foundation::CGRect;

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
