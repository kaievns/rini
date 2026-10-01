//! What the input context can be told about itself. Plain data with defaults; the config file fills
//! it and hands the whole thing over as `InputSettings`.
use serde::Serialize;

use crate::input::domain::key::HotkeySpec;
use crate::input::platform::haptics::HapticPattern;

/// The workspace swipe, `gestures › swipe-workspace-switch`, and what both swipes share.
#[derive(Serialize, Debug, PartialEq, Clone)]
pub struct GestureSettings {
    pub enabled: bool,
    /// Consume the horizontal swipes rini handles, so macOS and the frontmost application do not
    /// also handle them.
    pub consume_dock_swipe: bool,
    pub invert_horizontal_swipe: bool,
    /// How far the fingers may drift vertically and still count as a horizontal swipe: a fraction,
    /// or a percentage above 1.
    pub swipe_vertical_tolerance: f64,
    /// Skip empty workspaces when a swipe switches workspace.
    pub skip_empty: bool,
    pub fingers: usize,
    /// Horizontal distance (0..1 of the trackpad) a switch needs.
    pub distance_pct: f64,
    pub haptics_enabled: bool,
    pub haptic_pattern: HapticPattern,
}

impl Default for GestureSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            consume_dock_swipe: true,
            invert_horizontal_swipe: false,
            swipe_vertical_tolerance: 0.4,
            skip_empty: true,
            fingers: 3,
            distance_pct: 0.08,
            haptics_enabled: true,
            haptic_pattern: HapticPattern::LevelChange,
        }
    }
}

#[derive(Serialize, Debug, PartialEq, Clone, Copy)]
pub struct WindowSnappingSettings {
    /// How much two windows must overlap, as intersection over union, before a drag swaps them.
    pub drag_swap_fraction: f64,
}

impl Default for WindowSnappingSettings {
    fn default() -> Self { Self { drag_swap_fraction: 0.3 } }
}

/// The strip-scrolling gesture, as the tiling context configures it (`gestures › swipe-view-scroll`).
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
