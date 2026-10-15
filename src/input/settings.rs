//! What the input context can be told about itself. Plain data with defaults; the config file fills
//! it and hands the whole thing over as `InputSettings`.
use serde::Serialize;

#[derive(Serialize, Debug, PartialEq, Clone, Copy)]
pub struct WindowSnappingSettings {
    /// How much two windows must overlap, as intersection over union, before a drag swaps them.
    pub drag_swap_fraction: f64,
}

impl Default for WindowSnappingSettings {
    fn default() -> Self { Self { drag_swap_fraction: 0.3 } }
}

/// Everything the session tap needs, assembled by the application from the config file.
#[derive(Debug, Clone, PartialEq)]
pub struct InputSettings {
    pub mouse_hides_on_focus: bool,
    pub focus_follows_mouse: bool,
}
