//! What the input context can be told about itself. Plain data with defaults and validation; the
//! config file fills it and hands the whole thing over as `InputSettings`.
use serde::{Deserialize, Serialize};

use crate::input::domain::key::HotkeySpec;
use crate::input::platform::haptics::HapticPattern;

fn yes() -> bool {
    true
}
fn no() -> bool {
    false
}
pub fn default_swipe_vertical_tolerance() -> f64 {
    0.4
}
pub fn default_swipe_fingers() -> usize {
    3
}
pub fn default_distance_pct() -> f64 {
    0.08
}

#[derive(Serialize, Deserialize, Debug, PartialEq, Clone)]
#[serde(deny_unknown_fields)]
pub struct GestureSettings {
    #[serde(default = "no")]
    pub enabled: bool,
    /// If true, consume horizontal swipe events owned by Rini so macOS and the
    /// foreground app do not also handle them.
    #[serde(default = "yes")]
    pub consume_dock_swipe: bool,
    #[serde(default)]
    pub invert_horizontal_swipe: bool,
    /// Maximum absolute Y delta allowed for the gesture to count as horizontal
    #[serde(default = "default_swipe_vertical_tolerance")]
    pub swipe_vertical_tolerance: f64,
    /// If true, attempt to skip empty workspaces on swipe (if supported)
    #[serde(default)]
    pub skip_empty: bool,
    #[serde(default = "default_swipe_fingers")]
    pub fingers: usize,
    /// Normalized horizontal distance (0..1) required to fire a swipe
    #[serde(default = "default_distance_pct")]
    pub distance_pct: f64,
    #[serde(default = "yes")]
    pub haptics_enabled: bool,
    #[serde(default)]
    pub haptic_pattern: HapticPattern,
}
impl Default for GestureSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            consume_dock_swipe: true,
            invert_horizontal_swipe: false,
            swipe_vertical_tolerance: default_swipe_vertical_tolerance(),
            skip_empty: true,
            fingers: default_swipe_fingers(),
            distance_pct: default_distance_pct(),
            haptics_enabled: true,
            haptic_pattern: HapticPattern::LevelChange,
        }
    }
}
impl GestureSettings {
    pub fn validate(&self) -> Vec<String> {
        let mut issues = Vec::new();
        if self.swipe_vertical_tolerance < 0.0 {
            issues.push(format!(
                "gestures.swipe_vertical_tolerance must be non-negative, got {}",
                self.swipe_vertical_tolerance
            ));
        }
        issues
    }
}

#[derive(Serialize, Deserialize, Debug, PartialEq, Clone, Default, Copy)]
#[serde(deny_unknown_fields)]
pub struct WindowSnappingSettings {
    #[serde(default = "default_drag_swap_fraction")]
    pub drag_swap_fraction: f64,
}
fn default_drag_swap_fraction() -> f64 {
    0.3
}

/// The strip-scrolling gesture, as the tiling context configures it (`[settings.layout.scrolling.gestures]`).
/// Copied here by the application so this context does not read tiling's settings.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct ScrollGestureSettings {
    pub enabled: bool,
    pub invert_horizontal: bool,
    pub vertical_tolerance: f64,
    pub fingers: usize,
    pub distance_pct: f64,
}

/// Everything the two taps need, assembled by the application from the config file.
#[derive(Debug, Clone, PartialEq)]
pub struct InputSettings {
    pub mouse_hides_on_focus: bool,
    pub focus_follows_mouse: bool,
    pub focus_follows_mouse_disable_hotkey: Option<HotkeySpec>,
    pub gestures: GestureSettings,
    pub strip_scroll: ScrollGestureSettings,
}
