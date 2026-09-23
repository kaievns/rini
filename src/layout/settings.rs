//! What a strip can be told about itself: gaps, column widths, insertion, the scrolling gesture.
//! Plain data with defaults and validation; the config file fills it.
use serde::{Deserialize, Serialize};

fn no() -> bool {
    false
}
use rustc_hash::FxHashMap as HashMap;

fn default_scrolling_column_width_ratio() -> f64 {
    0.7
}
fn default_scrolling_min_column_width_ratio() -> f64 {
    0.3
}
fn default_scrolling_max_column_width_ratio() -> f64 {
    0.9
}
/// Preset column widths cycled by `cycle_preset_column_width`, mirroring niri's
/// `layout.preset-column-widths` defaults: a third, a half, two thirds.
fn default_scrolling_preset_column_widths() -> Vec<f64> {
    vec![0.33333, 0.5, 0.66667]
}
#[derive(Serialize, Deserialize, Debug, PartialEq, Clone, Copy, Default)]
#[serde(rename_all = "snake_case")]
pub enum WindowInsertionPoint {
    #[default]
    NextToSelection,
    EndOfTree,
}
/// Options flattened into both `[settings.layout]` and `[settings.layout.scrolling]`.
/// The scrolling value overrides the layout-wide value.
#[derive(Serialize, Deserialize, Debug, PartialEq, Clone, Default)]
#[serde(deny_unknown_fields)]
pub struct BaseLayoutSettings {
    #[serde(default)]
    pub window_insertion_point: Option<WindowInsertionPoint>,
}
#[derive(Serialize, Deserialize, Debug, PartialEq, Clone, Default)]
#[serde(deny_unknown_fields)]
pub struct LayoutSettings {
    /// Settings inherited by `scrolling` unless overridden in its table.
    #[serde(flatten)]
    pub base: BaseLayoutSettings,
    #[serde(default)]
    pub gaps: GapSettings,
    #[serde(default)]
    pub scrolling: ScrollingLayoutSettings,
}
#[derive(Serialize, Deserialize, Debug, PartialEq, Clone)]
#[serde(deny_unknown_fields)]
pub struct ScrollingLayoutSettings {
    #[serde(flatten)]
    pub base: BaseLayoutSettings,
    #[serde(default = "default_scrolling_column_width_ratio")]
    pub column_width_ratio: f64,
    #[serde(default = "default_scrolling_min_column_width_ratio")]
    pub min_column_width_ratio: f64,
    #[serde(default = "default_scrolling_max_column_width_ratio")]
    pub max_column_width_ratio: f64,
    /// Column widths cycled by `cycle_preset_column_width`, as ratios of the
    /// tiling width. Mirrors niri's `layout.preset-column-widths`.
    #[serde(default = "default_scrolling_preset_column_widths")]
    pub preset_column_widths: Vec<f64>,
    /// When true, horizontal focus stops at the ends of a display's strip instead
    /// of continuing onto the adjacent display. Each display then behaves as an
    /// isolated strip, which is what niri does with its per-output workspaces.
    #[serde(default)]
    pub isolate_displays: bool,
    #[serde(default)]
    pub alignment: ScrollingAlignment,
    /// Horizontal focus navigation behavior:
    /// - niri: reveal only as needed based on navigation direction.
    /// - anchored: always align focused column to `alignment`.
    #[serde(default)]
    pub focus_navigation_style: ScrollingFocusNavigationStyle,
    #[serde(default)]
    pub gestures: ScrollingGestureSettings,
}
impl Default for ScrollingLayoutSettings {
    fn default() -> Self {
        Self {
            base: BaseLayoutSettings::default(),
            column_width_ratio: default_scrolling_column_width_ratio(),
            min_column_width_ratio: default_scrolling_min_column_width_ratio(),
            max_column_width_ratio: default_scrolling_max_column_width_ratio(),
            preset_column_widths: default_scrolling_preset_column_widths(),
            isolate_displays: false,
            alignment: ScrollingAlignment::default(),
            focus_navigation_style: ScrollingFocusNavigationStyle::default(),
            gestures: ScrollingGestureSettings::default(),
        }
    }
}
#[derive(Serialize, Deserialize, Debug, PartialEq, Clone, Copy, Default)]
#[serde(rename_all = "snake_case")]
pub enum ScrollingAlignment {
    Left,
    #[default]
    Center,
    Right,
}
#[derive(Serialize, Deserialize, Debug, PartialEq, Clone, Copy, Default)]
#[serde(rename_all = "snake_case")]
pub enum ScrollingFocusNavigationStyle {
    #[default]
    Niri,
    Anchored,
}
#[derive(Serialize, Deserialize, Debug, PartialEq, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub struct ScrollingGestureSettings {
    #[serde(default = "no")]
    pub enabled: bool,
    #[serde(default)]
    pub invert_horizontal: bool,
    /// Maximum absolute Y delta allowed for the gesture to count as horizontal
    #[serde(default = "default_swipe_vertical_tolerance")]
    pub vertical_tolerance: f64,
    #[serde(default = "default_swipe_fingers")]
    pub fingers: usize,
    /// Normalized horizontal distance (0..1) required to fire a scroll step
    #[serde(default = "default_distance_pct")]
    pub distance_pct: f64,
    /// If true, scrolling past the end of the strip will trigger a workspace switch
    #[serde(default = "no")]
    pub propagate_to_workspace_swipe: bool,
    /// Amount of overscroll (in steps) required to trigger a workspace switch
    #[serde(default = "default_overscroll_threshold")]
    pub workspace_switch_threshold: f64,
}
impl Default for ScrollingGestureSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            invert_horizontal: false,
            vertical_tolerance: default_swipe_vertical_tolerance(),
            fingers: default_swipe_fingers(),
            distance_pct: default_distance_pct(),
            propagate_to_workspace_swipe: false,
            workspace_switch_threshold: default_overscroll_threshold(),
        }
    }
}
#[derive(Serialize, Deserialize, Debug, PartialEq, Clone, Default)]
#[serde(deny_unknown_fields)]
pub struct GapSettings {
    #[serde(default)]
    pub outer: OuterGaps,
    #[serde(default)]
    pub inner: InnerGaps,
    #[serde(default)]
    pub per_display: HashMap<String, GapOverride>,
}
#[derive(Serialize, Deserialize, Debug, PartialEq, Clone, Default)]
#[serde(deny_unknown_fields)]
pub struct OuterGaps {
    #[serde(default)]
    pub top: f64,
    #[serde(default)]
    pub left: f64,
    #[serde(default)]
    pub bottom: f64,
    #[serde(default)]
    pub right: f64,
}
#[derive(Serialize, Deserialize, Debug, PartialEq, Clone, Default)]
#[serde(deny_unknown_fields)]
pub struct InnerGaps {
    #[serde(default)]
    pub horizontal: f64,
    #[serde(default)]
    pub vertical: f64,
}
#[derive(Serialize, Deserialize, Debug, PartialEq, Clone, Default)]
#[serde(deny_unknown_fields)]
pub struct GapOverride {
    #[serde(default)]
    pub outer: Option<OuterGaps>,
    #[serde(default)]
    pub inner: Option<InnerGaps>,
}
impl LayoutSettings {
    pub fn window_insertion_point(&self) -> WindowInsertionPoint {
        self.scrolling
            .base
            .window_insertion_point
            .or(self.base.window_insertion_point)
            .unwrap_or_default()
    }

    pub fn resolved_base(&self) -> BaseLayoutSettings {
        BaseLayoutSettings {
            window_insertion_point: Some(self.window_insertion_point()),
        }
    }

    pub fn validate(&self) -> Vec<String> {
        let mut issues = Vec::new();

        issues.extend(self.gaps.validate());

        issues.extend(self.scrolling.validate());

        issues
    }
}
impl ScrollingLayoutSettings {
    pub fn validate(&self) -> Vec<String> {
        let mut issues = Vec::new();

        if !(0.0..=1.0).contains(&self.column_width_ratio) {
            issues.push(format!(
                "layout.scrolling.column_width_ratio must be between 0.0 and 1.0, got {}",
                self.column_width_ratio
            ));
        }

        if !(0.0..=1.0).contains(&self.min_column_width_ratio) {
            issues.push(format!(
                "layout.scrolling.min_column_width_ratio must be between 0.0 and 1.0, got {}",
                self.min_column_width_ratio
            ));
        }

        if !(0.0..=1.0).contains(&self.max_column_width_ratio) {
            issues.push(format!(
                "layout.scrolling.max_column_width_ratio must be between 0.0 and 1.0, got {}",
                self.max_column_width_ratio
            ));
        }

        if self.min_column_width_ratio > self.max_column_width_ratio {
            issues.push(format!(
                "layout.scrolling.min_column_width_ratio ({}) must be <= max_column_width_ratio ({})",
                self.min_column_width_ratio, self.max_column_width_ratio
            ));
        }

        if !(self.min_column_width_ratio..=self.max_column_width_ratio)
            .contains(&self.column_width_ratio)
        {
            issues.push(format!(
                "layout.scrolling.column_width_ratio ({}) must be within min/max bounds",
                self.column_width_ratio
            ));
        }

        if self.gestures.vertical_tolerance < 0.0 {
            issues.push(format!(
                "layout.scrolling.gestures.vertical_tolerance must be non-negative, got {}",
                self.gestures.vertical_tolerance
            ));
        }

        issues
    }
}
impl GapSettings {
    pub fn validate(&self) -> Vec<String> {
        let mut issues = Vec::new();

        issues.extend(self.outer.validate());

        issues.extend(self.inner.validate());

        for (uuid, overrides) in &self.per_display {
            if let Some(outer) = &overrides.outer {
                for issue in outer.validate() {
                    issues.push(format!("per_display[{uuid}] {issue}"));
                }
            }
            if let Some(inner) = &overrides.inner {
                for issue in inner.validate() {
                    issues.push(format!("per_display[{uuid}] {issue}"));
                }
            }
        }

        issues
    }

    pub fn effective_for_display(&self, display_uuid: Option<&str>) -> GapSettings {
        let mut resolved = GapSettings {
            outer: self.outer.clone(),
            inner: self.inner.clone(),
            per_display: HashMap::default(),
        };
        if let Some(uuid) = display_uuid {
            if let Some(overrides) = self.per_display.get(uuid) {
                if let Some(outer_override) = &overrides.outer {
                    resolved.outer = outer_override.clone();
                }
                if let Some(inner_override) = &overrides.inner {
                    resolved.inner = inner_override.clone();
                }
            }
        }
        resolved
    }
}
impl OuterGaps {
    pub fn validate(&self) -> Vec<String> {
        let mut issues = Vec::new();

        if self.top < 0.0 {
            issues.push(format!("outer.top gap must be non-negative, got {}", self.top));
        }

        if self.left < 0.0 {
            issues.push(format!("outer.left gap must be non-negative, got {}", self.left));
        }

        if self.bottom < 0.0 {
            issues.push(format!(
                "outer.bottom gap must be non-negative, got {}",
                self.bottom
            ));
        }

        if self.right < 0.0 {
            issues.push(format!(
                "outer.right gap must be non-negative, got {}",
                self.right
            ));
        }

        issues
    }
}
impl InnerGaps {
    pub fn validate(&self) -> Vec<String> {
        let mut issues = Vec::new();

        if self.horizontal < 0.0 {
            issues.push(format!(
                "inner.horizontal gap must be non-negative, got {}",
                self.horizontal
            ));
        }

        if self.vertical < 0.0 {
            issues.push(format!(
                "inner.vertical gap must be non-negative, got {}",
                self.vertical
            ));
        }

        issues
    }
}
fn default_swipe_vertical_tolerance() -> f64 {
    0.4
}
fn default_swipe_fingers() -> usize {
    3
}
fn default_distance_pct() -> f64 {
    0.08
}
fn default_overscroll_threshold() -> f64 {
    0.15
}
