//! The config file: schema, parsing with suggestions, validation, save. Defaults and the
//! documented shape live in `rini.default.toml`, which is embedded here.

use std::path::{Path, PathBuf};
use std::str::FromStr;

use anyhow::bail;
pub use rini_protocol::{ConfigCommand, WorkspaceSelector};
use serde::{Deserialize, Serialize};

use rini_shared::collections::HashMap;
use rini_macos::hotkey::{Hotkey, HotkeySpec};

pub mod actor;
pub mod commands;
pub mod watcher;

pub use commands::{Command, ExecCmd, WmCmd, WmCommand};
pub use rini_macos::haptics::HapticPattern;

pub const MAX_WORKSPACES: usize = 128;

pub fn data_dir() -> PathBuf {
    dirs::home_dir().unwrap().join(".rini")
}
pub fn restore_file() -> PathBuf {
    data_dir().join("layout.ron")
}
pub fn config_file() -> PathBuf {
    dirs::home_dir().unwrap().join(".config").join("rini").join("config.toml")
}

#[derive(Serialize, Deserialize, Debug, PartialEq, Clone)]
#[serde(deny_unknown_fields)]
pub struct VirtualWorkspaceSettings {
    #[serde(default = "yes")]
    pub enabled: bool,
    #[serde(default = "default_workspace_count")]
    pub default_workspace_count: usize,
    #[serde(default = "no")]
    pub workspace_auto_back_and_forth: bool,
    #[serde(default, alias = "prevent_wrapping_around")]
    pub prevent_wrapping: bool,
    #[serde(default = "default_workspace_names")]
    pub workspace_names: Vec<String>,
    #[serde(default)]
    pub default_workspace: usize,
    #[serde(default)]
    pub reapply_app_rules_on_title_change: bool,
    /// Modal windows (`AXModal`) float instead of taking a column. A rule that names `modal`
    /// overrides this for the windows it matches.
    #[serde(default = "yes")]
    pub float_modal_windows: bool,
    #[serde(default)]
    pub app_rules: Vec<AppWorkspaceRule>,
}

#[derive(Serialize, Deserialize, Debug, PartialEq, Clone)]
#[serde(deny_unknown_fields)]
pub struct AppWorkspaceRule {
    pub app_id: Option<String>,
    /// Target workspace index (0 based) OR workspace name. If None, window goes to active workspace.
    pub workspace: Option<WorkspaceSelector>,
    #[serde(default)]
    pub floating: bool,
    /// Initial normalized position for a floating window. `(0, 0)` is the top-left
    /// and `(1, 1)` is the bottom-right of the available screen area.
    pub position: Option<AppRulePosition>,
    /// Preferred window size in logical pixels.
    pub size: Option<AppRuleSize>,
    /// Focus the window after applying this rule, switching virtual workspaces if needed.
    #[serde(default)]
    pub focus: bool,
    /// Whether Rini should manage matching windows (defaults to true). `false` makes the
    /// window invisible to Rini (no tiling, floating, or assignments).
    #[serde(default = "yes")]
    pub manage: bool,
    pub app_name: Option<String>,
    pub title_regex: Option<String>,
    /// Matched as a literal substring of the title; `title_regex` for anything else.
    pub title_substring: Option<String>,

    /// Exact match on `AXRole`.
    pub ax_role: Option<String>,

    /// Exact match on `AXSubrole`.
    pub ax_subrole: Option<String>,

    /// Optional: match on the window's `AXModal` attribute. `true` matches modal dialogs,
    /// `false` matches everything else. A rule naming `modal` overrides `float_modal_windows`.
    pub modal: Option<bool>,
}

#[derive(Serialize, Deserialize, Debug, PartialEq, Clone, Copy)]
#[serde(deny_unknown_fields)]
pub struct AppRulePosition {
    pub x: f64,
    pub y: f64,
}

#[derive(Serialize, Deserialize, Debug, PartialEq, Clone, Copy)]
#[serde(deny_unknown_fields)]
pub struct AppRuleSize {
    pub w: Option<f64>,
    pub h: Option<f64>,
}

impl Default for VirtualWorkspaceSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            default_workspace_count: default_workspace_count(),
            workspace_auto_back_and_forth: false,
            prevent_wrapping: false,
            workspace_names: default_workspace_names(),
            default_workspace: 0,
            reapply_app_rules_on_title_change: false,
            float_modal_windows: true,
            app_rules: Vec::new(),
        }
    }
}

impl VirtualWorkspaceSettings {
    pub fn validate(&self) -> Vec<String> {
        let mut issues = Vec::new();

        if self.default_workspace_count == 0 {
            issues.push("default_workspace_count must be at least 1".to_string());
        }
        if self.default_workspace_count > MAX_WORKSPACES {
            issues.push(format!(
                "default_workspace_count should not exceed {} for performance reasons",
                MAX_WORKSPACES
            ));
        }

        if self.workspace_names.len() > self.default_workspace_count {
            issues.push("More workspace names provided than default_workspace_count".to_string());
        }

        if self.default_workspace >= self.default_workspace_count {
            issues.push(format!(
                "default_workspace ({}) must be less than default_workspace_count ({})",
                self.default_workspace, self.default_workspace_count
            ));
        }

        // Validate rules and check duplicates in a single pass
        let mut seen_app_ids = rini_shared::collections::HashSet::default();
        let mut seen_app_names = rini_shared::collections::HashSet::default();
        let mut seen_title_regexes = rini_shared::collections::HashSet::default();
        let mut seen_title_substrings = rini_shared::collections::HashSet::default();
        let mut seen_ax_roles = rini_shared::collections::HashSet::default();
        let mut seen_ax_subroles = rini_shared::collections::HashSet::default();

        for (index, rule) in self.app_rules.iter().enumerate() {
            let app_id_empty = rule.app_id.as_ref().map_or(true, |id| id.is_empty());
            if app_id_empty
                && rule.app_name.is_none()
                && rule.title_regex.is_none()
                && rule.title_substring.is_none()
                && rule.ax_role.is_none()
                && rule.ax_subrole.is_none()
                && rule.modal.is_none()
            {
                issues.push(format!(
                    "App rule {} has no app_id, app_name, title_regex, title_substring, ax_role, ax_subrole, or modal specified",
                    index
                ));
            }

            if let Some(ref workspace) = rule.workspace {
                if let WorkspaceSelector::Index(idx) = workspace {
                    if *idx >= self.default_workspace_count {
                        issues.push(format!(
                            "App rule {} references workspace {} but only {} workspaces will be created",
                            index, idx, self.default_workspace_count
                        ));
                    }
                }
            }

            if let Some(position) = rule.position {
                if !position.x.is_finite()
                    || !position.y.is_finite()
                    || !(0.0..=1.0).contains(&position.x)
                    || !(0.0..=1.0).contains(&position.y)
                {
                    issues.push(format!(
                        "App rule {} position x and y must be finite values between 0 and 1",
                        index
                    ));
                }
                if !rule.floating {
                    issues.push(format!(
                        "App rule {} specifies position, but position only applies when floating = true",
                        index
                    ));
                }
            }

            if let Some(size) = rule.size {
                if size.w.is_none() && size.h.is_none() {
                    issues.push(format!(
                        "App rule {} size must specify at least one of w or h",
                        index
                    ));
                }
                if size.w.is_some_and(|value| !value.is_finite() || value <= 0.0)
                    || size.h.is_some_and(|value| !value.is_finite() || value <= 0.0)
                {
                    issues.push(format!(
                        "App rule {} size dimensions must be finite positive values",
                        index
                    ));
                }
            }

            if let Some(ref app_id) = rule.app_id {
                if !app_id.is_empty() && !app_id.contains('.') {
                    issues.push(format!(
                        "App rule {} has suspicious app_id '{}' (should be bundle identifier like 'com.example.app')",
                        index, app_id
                    ));
                }

                let has_specific_match = rule.app_name.is_some()
                    || rule.title_regex.is_some()
                    || rule.title_substring.is_some()
                    || rule.ax_role.is_some()
                    || rule.ax_subrole.is_some();
                if !app_id.is_empty() && !has_specific_match && !seen_app_ids.insert(app_id) {
                    issues.push(format!("Duplicate app_id '{}' in rule {}", app_id, index));
                }
            }

            if let Some(ref app_name) = rule.app_name {
                if !seen_app_names.insert(app_name) {
                    issues.push(format!("Duplicate app_name '{}' in rule {}", app_name, index));
                }
            }

            if let Some(ref title_re) = rule.title_regex {
                if title_re.is_empty() {
                    issues.push(format!("App rule {} has empty title_regex", index));
                } else if !seen_title_regexes.insert(title_re) {
                    issues.push(format!("Duplicate title_regex '{}' in rule {}", title_re, index));
                }
            }

            if let Some(ref title_sub) = rule.title_substring {
                if title_sub.is_empty() {
                    issues.push(format!("App rule {} has empty title_substring", index));
                } else if !seen_title_substrings.insert(title_sub) {
                    issues.push(format!(
                        "Duplicate title_substring '{}' in rule {}",
                        title_sub, index
                    ));
                }
            }

            if let Some(ref ax_role) = rule.ax_role {
                if ax_role.is_empty() {
                    issues.push(format!("App rule {} has empty ax_role", index));
                } else if !seen_ax_roles.insert(ax_role) {
                    issues.push(format!("Duplicate ax_role '{}' in rule {}", ax_role, index));
                }
            }

            if let Some(ref ax_sub) = rule.ax_subrole {
                if ax_sub.is_empty() {
                    issues.push(format!("App rule {} has empty ax_subrole", index));
                } else if !seen_ax_subroles.insert(ax_sub) {
                    issues.push(format!("Duplicate ax_subrole '{}' in rule {}", ax_sub, index));
                }
            }
        }

        issues
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ConfigFile {
    settings: Settings,
    keys: HashMap<String, WmCommand>,
    #[serde(default)]
    virtual_workspaces: VirtualWorkspaceSettings,
    /// Modifier combinations that can be reused in key bindings
    /// e.g., "comb1" = "Alt + Shift" allows using "comb1 + C" in keys
    #[serde(default)]
    modifier_combinations: HashMap<String, String>,
}

fn migrate_legacy_resize_bindings(document: &mut toml::Value) -> bool {
    let Some(keys) = document.get_mut("keys").and_then(toml::Value::as_table_mut) else {
        return false;
    };

    let mut migrated = false;
    for (_, command) in keys.iter_mut() {
        let legacy_name = match command.as_str() {
            Some("resize_window_grow") => "resize_window_grow",
            Some("resize_window_shrink") => "resize_window_shrink",
            _ => continue,
        };
        *command = toml::Value::Table(toml::map::Map::from_iter([(
            legacy_name.to_string(),
            toml::Value::String("horizontal".to_string()),
        )]));
        migrated = true;
    }
    migrated
}

fn parse_config_file(buf: &str) -> Result<ConfigFile, toml::de::Error> {
    toml::from_str(buf).or_else(|original_error| {
        let Ok(mut document) = toml::from_str::<toml::Value>(buf) else {
            return Err(original_error);
        };
        if !migrate_legacy_resize_bindings(&mut document) {
            return Err(original_error);
        }
        document.try_into()
    })
}

#[derive(Serialize, Clone, Debug)]
pub struct Config {
    pub settings: Settings,
    pub keys: Vec<(Hotkey, WmCommand)>,
    #[serde(default)]
    pub key_specs: Vec<(String, WmCommand)>,
    pub virtual_workspaces: VirtualWorkspaceSettings,
}

impl<'de> Deserialize<'de> for Config {
    fn deserialize<D>(deserializer: D) -> Result<Config, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct ConfigSerde {
            settings: Settings,
            keys: Vec<(Hotkey, WmCommand)>,
            #[serde(default)]
            key_specs: Vec<(String, WmCommand)>,
            virtual_workspaces: VirtualWorkspaceSettings,
        }

        let config = ConfigSerde::deserialize(deserializer)?;
        let key_specs = if config.key_specs.is_empty() && !config.keys.is_empty() {
            config
                .keys
                .iter()
                .map(|(hotkey, command)| (hotkey.to_string(), command.clone()))
                .collect()
        } else {
            config.key_specs
        };

        Ok(Config {
            settings: config.settings,
            keys: config.keys,
            key_specs,
            virtual_workspaces: config.virtual_workspaces,
        })
    }
}

unsafe impl Send for Config {}
unsafe impl Sync for Config {}

/// Which side of the desk the logically-upper display sits on; macOS cannot tell us. Semantics in
/// `rini.default.toml` under `stacked_display_upper_is`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum StackedUpperSide {
    #[default]
    Left,
    Right,
}

#[derive(Serialize, Deserialize, Debug, PartialEq, Clone)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    /// Whether layout changes, strip movements and workspace switches are animated. Everything
    /// animates through the overlay (`docs/animation-smoothness.md`); off places windows at once.
    #[serde(default = "yes")]
    pub animate: bool,
    #[serde(default = "default_animation_duration")]
    pub animation_duration: f64,
    #[serde(default = "yes")]
    pub default_disable: bool,
    #[serde(default = "yes")]
    pub mouse_follows_focus: bool,
    #[serde(default = "yes")]
    pub mouse_hides_on_focus: bool,
    // The three stacked-display keys are explained where the user reads them: rini.default.toml.
    #[serde(default = "no")]
    pub warp_cursor_between_stacked_displays: bool,
    #[serde(default)]
    pub stacked_display_upper_is: StackedUpperSide,
    /// Fraction up the upper display's height at which the lower display's top edge sits.
    #[serde(default = "default_stacked_lower_top_at")]
    pub stacked_display_lower_top_at: f64,
    #[serde(default = "yes")]
    pub focus_follows_mouse: bool,
    /// Held to suspend focus-follows-mouse; a full hotkey or a modifier-only spec.
    #[serde(default)]
    pub focus_follows_mouse_disable_hotkey: Option<HotkeySpec>,
    /// Bundle ids whose activation must not switch workspaces (Spotlight-style focus stealers).
    #[serde(default)]
    pub auto_focus_blacklist: Vec<String>,
    #[serde(default)]
    pub layout: LayoutSettings,
    #[serde(default)]
    pub gestures: GestureSettings,

    #[serde(default)]
    pub window_snapping: WindowSnappingSettings,

    #[serde(default)]
    pub run_on_start: Vec<String>,

    #[serde(default = "yes")]
    pub hot_reload: bool,
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

#[derive(Serialize, Deserialize, Debug, PartialEq, Clone, Default, Copy)]
#[serde(deny_unknown_fields)]
pub struct WindowSnappingSettings {
    #[serde(default = "default_drag_swap_fraction")]
    pub drag_swap_fraction: f64,
}

fn default_drag_swap_fraction() -> f64 {
    0.3
}

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

impl Settings {
    pub fn validate(&self) -> Vec<String> {
        let mut issues = Vec::new();

        if self.animation_duration < 0.0 {
            issues.push(format!(
                "animation_duration must be non-negative, got {}",
                self.animation_duration
            ));
        }


        issues.extend(self.layout.validate());

        if self.gestures.swipe_vertical_tolerance < 0.0 {
            issues.push(format!(
                "gestures.swipe_vertical_tolerance must be non-negative, got {}",
                self.gestures.swipe_vertical_tolerance
            ));
        }

        issues
    }
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

fn yes() -> bool {
    true
}


fn default_animation_duration() -> f64 {
    0.35
}


#[allow(dead_code)]
pub fn default_stacked_lower_top_at() -> f64 {
    // The laptop's top edge a little under halfway up the larger screen, which is where a laptop
    // parked beside a big monitor tends to sit.
    0.4
}

fn no() -> bool {
    false
}

fn default_workspace_count() -> usize {
    4
}

fn default_workspace_names() -> Vec<String> {
    vec![
        "Main".to_string(),
        "Development".to_string(),
        "Communication".to_string(),
        "Utilities".to_string(),
    ]
}

// Interpreted as normalized fraction when <= 1.0. If > 1.0 and <= 100.0,
// it is treated as a percentage (e.g. 40.0 -> 0.40).
fn default_swipe_vertical_tolerance() -> f64 { 0.4 }
fn default_swipe_fingers() -> usize { 3 }
fn default_distance_pct() -> f64 { 0.08 }
fn default_overscroll_threshold() -> f64 { 0.15 }


impl Config {
    pub fn read(path: &Path) -> anyhow::Result<Config> {
        let buf = std::fs::read_to_string(path)?;
        Self::parse(&buf)
    }

    pub fn default() -> Config {
        Self::parse(include_str!("../../../rini.default.toml")).unwrap()
    }

    /// Writes the config back out. Bindings are written expanded: `modifier_combinations`
    /// shorthands were resolved at parse time and the file keeps only the resolved keys, so
    /// the saved table is empty. Comments are not preserved either.
    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        let config_file = ConfigFile {
            settings: self.settings.clone(),
            keys: self
                .key_specs
                .iter()
                .map(|(hotkey, command)| (hotkey.clone(), command.clone()))
                .collect(),
            virtual_workspaces: self.virtual_workspaces.clone(),
            modifier_combinations: HashMap::default(),
        };

        let toml_string = toml::to_string_pretty(&config_file)?;
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        std::fs::write(path, toml_string.as_bytes())?;

        Ok(())
    }

    pub fn validate(&self) -> Vec<String> {
        let mut issues = Vec::new();

        issues.extend(self.settings.validate());

        issues.extend(self.virtual_workspaces.validate());

        issues
    }

    fn normalize_hotkey_string(key: &str) -> String {
        let mut out = String::with_capacity(key.len());
        let mut word = String::new();

        for ch in key.chars() {
            if ch.is_alphabetic() {
                word.push(ch);
            } else {
                if !word.is_empty() {
                    let token = if word.len() == 1 {
                        word.to_ascii_uppercase()
                    } else {
                        match word.to_lowercase().as_str() {
                            "up" => "ArrowUp".to_string(),
                            "down" => "ArrowDown".to_string(),
                            "left" => "ArrowLeft".to_string(),
                            "right" => "ArrowRight".to_string(),
                            _ => word.clone(),
                        }
                    };
                    out.push_str(&token);
                    word.clear();
                }
                out.push(ch);
            }
        }

        if !word.is_empty() {
            let token = if word.len() == 1 {
                word.to_ascii_uppercase()
            } else {
                match word.to_lowercase().as_str() {
                    "up" => "ArrowUp".to_string(),
                    "down" => "ArrowDown".to_string(),
                    "left" => "ArrowLeft".to_string(),
                    "right" => "ArrowRight".to_string(),
                    _ => word.clone(),
                }
            };
            out.push_str(&token);
        }

        out
    }

    fn expand_modifier_combinations(key: &str, combinations: &HashMap<String, String>) -> String {
        if let Some(plus_pos) = key.find(" + ") {
            let potential_combo = &key[..plus_pos];
            if let Some(combo_value) = combinations.get(potential_combo) {
                let rest = &key[plus_pos + 3..];
                return format!("{} + {}", combo_value, rest);
            }
        }
        key.to_string()
    }

    /// no need to pull in a dep for just this
    fn levenshtein(a: &str, b: &str) -> usize {
        let a_chars: Vec<char> = a.chars().collect();
        let b_chars: Vec<char> = b.chars().collect();
        let mut d = vec![vec![0usize; b_chars.len() + 1]; a_chars.len() + 1];
        for i in 0..=a_chars.len() {
            d[i][0] = i;
        }
        for j in 0..=b_chars.len() {
            d[0][j] = j;
        }
        for i in 1..=a_chars.len() {
            for j in 1..=b_chars.len() {
                let cost = if a_chars[i - 1] == b_chars[j - 1] {
                    0
                } else {
                    1
                };
                d[i][j] = std::cmp::min(
                    std::cmp::min(d[i - 1][j] + 1, d[i][j - 1] + 1),
                    d[i - 1][j - 1] + cost,
                );
            }
        }
        d[a_chars.len()][b_chars.len()]
    }

    // Extracts an "unknown variant `...`" token from serde error string when present.
    // Additionally, if serde's error message contains an "expected" list (backtick-delimited),
    // embed those expected tokens alongside the unknown token using the separator "||".
    // The resulting returned string may therefore be:
    //   - "unknown_token" (no expected candidates found)
    //   - "unknown_token||cand1,cand2,..." (candidates appended)
    fn extract_unknown_variant(err: &str) -> Option<String> {
        let needle = "unknown variant `";
        if let Some(start) = err.find(needle) {
            let rest = &err[start + needle.len()..];
            if let Some(end) = rest.find('`') {
                let unknown = rest[..end].to_string();

                // Collect all backtick-enclosed tokens in the error message and
                // treat them as candidate variants (excluding the unknown itself).
                let mut variants: Vec<String> = Vec::new();
                let mut i = 0usize;
                while let Some(open) = err[i..].find('`') {
                    let open_abs = i + open + 1;
                    if let Some(close_off) = err[open_abs..].find('`') {
                        let close_abs = open_abs + close_off;
                        let token = &err[open_abs..close_abs];
                        if token != unknown {
                            variants.push(token.to_string());
                        }
                        i = close_abs + 1;
                    } else {
                        break;
                    }
                }

                if !variants.is_empty() {
                    // dedupe while preserving order
                    let mut seen = std::collections::HashSet::new();
                    let mut deduped = Vec::new();
                    for v in variants {
                        if seen.insert(v.clone()) {
                            deduped.push(v);
                        }
                    }
                    return Some(format!("{}||{}", unknown, deduped.join(",")));
                }

                return Some(unknown);
            }
        }

        if let Some(unknown_pos) = err.find("unknown") {
            if let Some(backtick_pos) = err[unknown_pos..].find('`') {
                let rest = &err[unknown_pos + backtick_pos + 1..];
                if let Some(end) = rest.find('`') {
                    return Some(rest[..end].to_string());
                }
            }
        }
        None
    }

    // Provide suggestion by comparing the unknown token to a list of known commands.
    // If the `unknown` string was produced by `extract_unknown_variant` and contains
    // an embedded serde candidate list (format: "token||cand1,cand2"), prefer those
    // candidates when computing the best suggestion. Otherwise fall back to the
    // conservative builtin list.
    //
    // Returns the best candidate if its distance is within a reasonable threshold.
    fn suggest_similar_command(unknown: &str) -> Option<String> {
        // Detect if `unknown` was augmented with serde-provided expected variants.
        let (unknown_token, serde_candidates): (String, Option<Vec<String>>) =
            if let Some(idx) = unknown.find("||") {
                let (u, rest) = unknown.split_at(idx);
                let rest = &rest[2..];
                let candidates: Vec<String> = rest
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
                (u.to_lowercase(), Some(candidates))
            } else {
                (unknown.to_lowercase(), None)
            };

        // Choose candidate set: prefer serde-provided ones when available.
        let mut best: Option<(String, usize)> = None;

        if let Some(cands) = serde_candidates {
            for cand in cands.iter() {
                let cand_norm = cand.to_lowercase();
                let dist = Self::levenshtein(&unknown_token, &cand_norm);
                if best.is_none() || dist < best.as_ref().unwrap().1 {
                    best = Some((cand.clone(), dist));
                }
            }
        } else {
            // Use dynamically generated builtin candidates.
            let builtin_candidates = WmCommand::builtin_candidates();
            for cand in builtin_candidates.iter() {
                let dist = Self::levenshtein(&unknown_token, &cand.to_lowercase());
                if best.is_none() || dist < best.as_ref().unwrap().1 {
                    best = Some((cand.to_string(), dist));
                }
            }
        }

        if let Some((best_cand, dist)) = best {
            // Heuristic threshold: allow suggestions if distance is <= half the length (or <=3).
            let threshold = std::cmp::max(3usize, best_cand.len() / 2);
            if dist <= threshold {
                return Some(best_cand);
            }
        }

        None
    }

    fn parse(buf: &str) -> anyhow::Result<Config> {
        // Attempt to deserialize. If it fails, and the error indicates an unknown enum
        // variant, attempt to provide a helpful suggestion.
        match parse_config_file(buf) {
            Ok(c) => {
                let mut keys = Vec::new();
                let mut key_specs = Vec::new();
                for (key, cmd) in c.keys {
                    let expanded_key =
                        Self::expand_modifier_combinations(&key, &c.modifier_combinations);
                    let normalized_key = Self::normalize_hotkey_string(&expanded_key);
                    let Ok(hotkey) = Hotkey::from_str(&normalized_key) else {
                        bail!("Could not parse hotkey: {key}");
                    };
                    keys.push((hotkey, cmd.clone()));
                    key_specs.push((normalized_key, cmd));
                }
                Ok(Config {
                    settings: c.settings,
                    keys,
                    key_specs,
                    virtual_workspaces: c.virtual_workspaces,
                })
            }
            Err(e) => {
                let msg = e.to_string();
                let unknown = Self::extract_unknown_variant(&msg)
                    .or_else(|| Self::first_unbindable_command(buf));
                match unknown.and_then(|token| Self::suggest_similar_command(&token)) {
                    Some(suggestion) => bail!("{msg}\nDid you mean `{}`?", suggestion),
                    None => bail!("{msg}"),
                }
            }
        }
    }

    /// `WmCommand` is `#[serde(untagged)]`, so a misspelt binding reports "did not match any
    /// variant" and names nothing. Find the offending string in `[keys]` so it can be suggested for.
    fn first_unbindable_command(buf: &str) -> Option<String> {
        let document: toml::Value = toml::from_str(buf).ok()?;
        let keys = document.get("keys")?.as_table()?;
        keys.values()
            .filter_map(toml::Value::as_str)
            .find(|command| serde_json::from_value::<WmCommand>(serde_json::Value::String(command.to_string())).is_err())
            .map(str::to_owned)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rini_protocol::{LayoutCommand, ResizeOrientation};

    #[test]
    fn scrolling_insertion_point_falls_back_to_the_global_default() {
        let overridden: LayoutSettings = toml::from_str(
            r#"
                window_insertion_point = "end_of_tree"

                [scrolling]
                window_insertion_point = "next_to_selection"
            "#,
        )
        .unwrap();
        assert_eq!(
            overridden.window_insertion_point(),
            WindowInsertionPoint::NextToSelection
        );

        let inherited: LayoutSettings = toml::from_str(
            r#"
                window_insertion_point = "end_of_tree"
            "#,
        )
        .unwrap();
        assert_eq!(
            inherited.window_insertion_point(),
            WindowInsertionPoint::EndOfTree
        );
    }

    #[test]
    fn virtual_workspace_prevent_wrapping_defaults_to_false_and_accepts_suggested_alias() {
        let defaults: VirtualWorkspaceSettings = toml::from_str("").unwrap();
        assert!(!defaults.prevent_wrapping);

        let settings: VirtualWorkspaceSettings =
            toml::from_str("prevent_wrapping_around = true").unwrap();
        assert!(settings.prevent_wrapping);
    }

    #[test]
    fn app_rules_parse_placement_size_and_focus() {
        let settings: VirtualWorkspaceSettings = toml::from_str(
            r#"
                app_rules = [{
                    app_id = "com.example.Tool",
                    floating = true,
                    position = { x = 0.4, y = 0.7 },
                    size = { w = 640, h = 480 },
                    focus = true
                }]
            "#,
        )
        .unwrap();

        let rule = &settings.app_rules[0];
        assert_eq!(rule.position, Some(AppRulePosition { x: 0.4, y: 0.7 }));
        assert_eq!(rule.size, Some(AppRuleSize { w: Some(640.0), h: Some(480.0) }));
        assert!(rule.focus);
        assert!(settings.validate().is_empty());

        let height_only: VirtualWorkspaceSettings = toml::from_str(
            r#"
                app_rules = [{
                    app_id = "com.example.Panel",
                    size = { h = 320 }
                }]
            "#,
        )
        .unwrap();
        assert_eq!(
            height_only.app_rules[0].size,
            Some(AppRuleSize { w: None, h: Some(320.0) })
        );
        assert!(height_only.validate().is_empty());
    }

    #[test]
    fn app_rule_geometry_validation_rejects_invalid_values() {
        let mut settings = VirtualWorkspaceSettings::default();
        settings.app_rules.push(AppWorkspaceRule {
            app_id: Some("com.example.Tool".into()),
            workspace: None,
            floating: false,
            position: Some(AppRulePosition { x: -0.1, y: 1.1 }),
            size: Some(AppRuleSize {
                w: Some(0.0),
                h: Some(f64::NAN),
            }),
            focus: false,
            manage: true,
            app_name: None,
            title_regex: None,
            title_substring: None,
            ax_role: None,
            ax_subrole: None,
            modal: None,
        });

        let issues = settings.validate();
        assert!(issues.iter().any(|issue| issue.contains("between 0 and 1")));
        assert!(issues.iter().any(|issue| issue.contains("only applies")));
        assert!(issues.iter().any(|issue| issue.contains("finite positive")));
    }

    #[test]
    fn resize_command_config_supports_legacy_and_oriented_forms() {
        #[derive(Deserialize)]
        struct TestConfig {
            keys: HashMap<String, WmCommand>,
        }

        let mut document: toml::Value = toml::from_str(
            r#"
            [keys]
            legacy = "resize_window_grow"
            vertical = { resize_window_shrink = "vertical" }
            smart = { resize_window_grow = "smart" }
            "#,
        )
        .unwrap();
        assert!(migrate_legacy_resize_bindings(&mut document));
        let config: TestConfig = document.try_into().unwrap();

        assert_eq!(
            config.keys["legacy"],
            WmCommand::ReactorCommand(Command::Layout(LayoutCommand::ResizeWindowGrow(
                ResizeOrientation::Horizontal
            )))
        );
        assert_eq!(
            config.keys["vertical"],
            WmCommand::ReactorCommand(Command::Layout(LayoutCommand::ResizeWindowShrink(
                ResizeOrientation::Vertical
            )))
        );
        assert_eq!(
            config.keys["smart"],
            WmCommand::ReactorCommand(Command::Layout(LayoutCommand::ResizeWindowGrow(
                ResizeOrientation::Smart
            )))
        );
    }

    #[test]
    fn test_normalize_hotkey_string() {
        assert_eq!(
            Config::normalize_hotkey_string("Alt + Shift + Down"),
            "Alt + Shift + ArrowDown"
        );
        assert_eq!(Config::normalize_hotkey_string("Ctrl + Up"), "Ctrl + ArrowUp");
        assert_eq!(
            Config::normalize_hotkey_string("Shift + Left"),
            "Shift + ArrowLeft"
        );
        assert_eq!(
            Config::normalize_hotkey_string("Meta + Right"),
            "Meta + ArrowRight"
        );
    }

    #[test]
    fn test_modifier_combinations_in_config() {
        let toml = r#"
            [settings]
            animate = false

            [modifier_combinations]
            comb1 = "Alt + Shift"
            leader = "Ctrl + Alt"

            [keys]
            "comb1 + C" = "toggle_space_activated"
            "leader + Tab" = "next_workspace"
            "Alt + H" = { move_focus = "left" }
        "#;

        let cfg = Config::parse(toml).unwrap();
        // We expect keys to be parsed into hotkeys
        assert!(!cfg.keys.is_empty());
    }

    #[test]
    fn serde_round_trip_preserves_key_specs() {
        let cfg = Config::default();
        assert!(!cfg.key_specs.is_empty());

        let json = serde_json::to_string(&cfg).unwrap();
        let round_tripped: Config = serde_json::from_str(&json).unwrap();

        assert_eq!(round_tripped.key_specs, cfg.key_specs);
    }

    #[test]
    fn serde_without_key_specs_reconstructs_from_keys() {
        let cfg = Config::default();
        let mut json = serde_json::to_value(&cfg).unwrap();
        json.as_object_mut().unwrap().remove("key_specs");

        let round_tripped: Config = serde_json::from_value(json).unwrap();

        assert_eq!(round_tripped.key_specs.len(), round_tripped.keys.len());
        assert!(!round_tripped.key_specs.is_empty());
    }

    #[test]
    fn test_levenshtein_suggests() {
        let err =
            "unknown variant `toggle_stak`, expected one of `toggle_stack`, `unjoin_windows`";
        let token = Config::extract_unknown_variant(err).unwrap();
        assert_eq!(token, "toggle_stak||toggle_stack,unjoin_windows");
        let suggestion = Config::suggest_similar_command(&token);
        assert_eq!(suggestion.as_deref(), Some("toggle_stack"));
    }
    #[test]
    fn the_default_config_validates_clean() {
        assert_eq!(Config::default().validate(), Vec::<String>::new());
    }

    #[test]
    fn workspace_settings_validation_names_each_rule_it_rejects() {
        let mut vw = VirtualWorkspaceSettings::default();
        vw.default_workspace_count = 2;
        vw.default_workspace = 2;
        vw.workspace_names = vec!["a".into(), "b".into(), "c".into()];
        let issues = vw.validate();
        assert!(issues.iter().any(|i| i.contains("default_workspace (2)")));
        assert!(issues.iter().any(|i| i.contains("More workspace names")));

        let mut too_many = VirtualWorkspaceSettings::default();
        too_many.default_workspace_count = MAX_WORKSPACES + 1;
        assert!(too_many.validate().iter().any(|i| i.contains("should not exceed")));
        let mut none = VirtualWorkspaceSettings::default();
        none.default_workspace_count = 0;
        assert!(none.validate().iter().any(|i| i.contains("at least 1")));
    }

    #[test]
    fn scrolling_width_ratios_must_nest() {
        let mut layout = LayoutSettings::default();
        layout.scrolling.min_column_width_ratio = 0.8;
        layout.scrolling.max_column_width_ratio = 0.5;
        layout.scrolling.column_width_ratio = 0.7;
        let issues = layout.validate();
        assert!(issues.iter().any(|i| i.contains("must be <= max_column_width_ratio")));
        assert!(issues.iter().any(|i| i.contains("within min/max bounds")));
        let mut settings = Config::default().settings;
        settings.animation_duration = -1.0;
        assert!(settings.validate().iter().any(|i| i.contains("animation_duration")));
    }

    #[test]
    fn save_then_read_gives_back_the_same_bindings() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let original = Config::default();
        original.save(&path).unwrap();
        let reread = Config::read(&path).unwrap();
        assert_eq!(reread.settings, original.settings);
        assert_eq!(reread.virtual_workspaces, original.virtual_workspaces);
        let mut a = original.key_specs.clone();
        let mut b = reread.key_specs.clone();
        a.sort_by(|x, y| x.0.cmp(&y.0));
        b.sort_by(|x, y| x.0.cmp(&y.0));
        assert_eq!(a, b);
    }

    #[test]
    fn an_unknown_key_in_the_file_is_an_error_with_the_key_named() {
        let err = Config::parse("[settings]\nno_such_key = 1\n[keys]\n").unwrap_err().to_string();
        assert!(err.contains("no_such_key"), "{err}");
    }

    #[test]
    fn a_misspelt_command_gets_a_suggestion() {
        let err = Config::parse(
            "[settings]\n[keys]\n\"Alt + Z\" = \"toggle_space_activate\"\n",
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("Did you mean `toggle_space_activated`"), "{err}");
    }

}
