//! The config: what rini runs on, read from `config.kdl`. Its defaults are the features' own
//! `Default` impls; `rini.default.kdl`, embedded here, documents them and is used whenever the file
//! is missing or cannot be used. The format and its decisions are in `docs/config.md`.

use std::path::Path;

pub use rini_ipc::protocol::{ConfigCommand, WorkspaceSelector};
use serde::Serialize;

use crate::app::startup::StartupCommand;
use crate::displays::platform::cursor_warp::StackedUpperSide;
use crate::input::domain::binding::Bind;
use crate::input::domain::key::{HotkeySpec, ModKey};
use crate::input::settings::{
    GestureSettings, InputSettings, ScrollGestureSettings, WindowSnappingSettings,
};
pub use crate::layout::settings::{Edges, LayoutSettings};
pub use crate::windows::domain::rules::{RuleMatch, WindowRule};
pub use crate::workspaces::settings::{MAX_WORKSPACES, VirtualWorkspaceSettings};

pub mod actor;
pub mod kdl;
pub mod watcher;

pub use kdl::error::{ConfigError, LoadError};

/// The reference config, compiled in.
pub const DEFAULT_KDL: &str = include_str!("../../../rini.default.kdl");

/// The movement animation's length at `slowdown 1.0`, in seconds.
pub const BASE_ANIMATION_SECONDS: f64 = 0.35;

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Config {
    pub settings: Settings,
    /// `binds`, in file order.
    pub binds: Vec<Bind>,
    /// `recent-windows › binds`: the switcher's keys.
    pub switcher_binds: Vec<Bind>,
    pub virtual_workspaces: VirtualWorkspaceSettings,
}

#[derive(Serialize, Debug, PartialEq, Clone)]
pub struct Settings {
    /// Layout changes, strip movements and workspace switches are drawn through the overlay
    /// (`src/transitions/docs/coordination.md`); off places windows at once.
    pub animate: bool,
    /// Seconds, `BASE_ANIMATION_SECONDS` times `animations › slowdown`.
    pub animation_duration: f64,
    pub default_disable: bool,
    pub mouse_follows_focus: bool,
    pub mouse_hides_on_focus: bool,
    pub warp_cursor_between_stacked_displays: bool,
    pub stacked_display_upper_is: StackedUpperSide,
    /// Fraction up the upper display's height at which the lower display's top edge sits.
    pub stacked_display_lower_top_at: f64,
    pub focus_follows_mouse: bool,
    /// Held to suspend focus-follows-mouse; a full hotkey or a modifier-only spec.
    pub focus_follows_mouse_disable_hotkey: Option<HotkeySpec>,
    /// Bundle ids whose activation must not switch workspaces (Spotlight-style focus stealers).
    pub auto_focus_denylist: Vec<String>,
    /// The modifier `Mod` stands for in a key.
    pub mod_key: ModKey,
    pub layout: LayoutSettings,
    pub gestures: GestureSettings,
    pub switcher: crate::switcher::settings::SwitcherSettings,
    pub bar: crate::bar::settings::BarSettings,
    /// `animations › journal`: whether every transition's journal is written.
    pub journal: crate::transitions::settings::JournalSettings,
    pub window_snapping: WindowSnappingSettings,
    pub run_on_start: Vec<StartupCommand>,
    /// Mark every bar while the config file cannot be used, until it can.
    pub mark_failed_config: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            animate: true,
            animation_duration: BASE_ANIMATION_SECONDS,
            default_disable: true,
            mouse_follows_focus: false,
            mouse_hides_on_focus: false,
            warp_cursor_between_stacked_displays: false,
            stacked_display_upper_is: StackedUpperSide::default(),
            // The laptop's top edge a little under halfway up the larger screen, which is where a
            // laptop parked beside a big monitor tends to sit.
            stacked_display_lower_top_at: 0.4,
            focus_follows_mouse: false,
            focus_follows_mouse_disable_hotkey: None,
            auto_focus_denylist: Vec::new(),
            mod_key: ModKey::default(),
            layout: LayoutSettings::default(),
            gestures: GestureSettings::default(),
            switcher: Default::default(),
            bar: Default::default(),
            journal: Default::default(),
            window_snapping: WindowSnappingSettings::default(),
            run_on_start: Vec::new(),
            mark_failed_config: true,
        }
    }
}

impl Settings {
    /// What every display keeps clear of windows at its top: the bar's height while it is drawn, and
    /// nothing when it is off, so another bar can have the band.
    pub fn top_band(&self) -> f64 {
        if self.bar.enabled {
            crate::bar::domain::layout::HEIGHT
        } else {
            0.0
        }
    }
}

impl Config {
    /// What every setting is when nothing says otherwise: no binds, no window rules.
    pub fn code_defaults() -> Config {
        Config {
            settings: Settings::default(),
            binds: Vec::new(),
            switcher_binds: Vec::new(),
            virtual_workspaces: VirtualWorkspaceSettings::default(),
        }
    }

    /// The embedded `rini.default.kdl`. A test parses it by name, so a broken one fails there first.
    pub fn default() -> Config {
        Self::parse("rini.default.kdl", DEFAULT_KDL)
            .unwrap_or_else(|error| panic!("{}", error.render()))
    }

    pub fn read(path: &Path) -> Result<Config, LoadError> {
        let text = std::fs::read_to_string(path).map_err(|error| LoadError::Unreadable {
            path: path.display().to_string(),
            error,
        })?;
        Ok(Self::parse(&path.display().to_string(), &text)?)
    }

    /// `text` laid over the code defaults. `name` is what the report calls it.
    pub fn parse(name: &str, text: &str) -> Result<Config, ConfigError> {
        Self::code_defaults().with(name, text)
    }

    /// `text` laid over this config, as `config set` applies a fragment. What does not decode and
    /// what the checks across nodes find come back in one report.
    pub fn with(&self, name: &str, text: &str) -> Result<Config, ConfigError> {
        let (part, mut problems) = kdl::parse(name, text)?;
        let mut config = self.clone();
        problems.extend(kdl::apply::apply(part, &mut config));
        if problems.is_empty() {
            Ok(config)
        } else {
            Err(ConfigError::new(name, text, problems))
        }
    }
}

impl From<&Config> for InputSettings {
    fn from(config: &Config) -> Self {
        let s = &config.settings;
        let g = &s.layout.scrolling.gestures;
        InputSettings {
            mouse_hides_on_focus: s.mouse_hides_on_focus,
            focus_follows_mouse: s.focus_follows_mouse,
            focus_follows_mouse_disable_hotkey: s.focus_follows_mouse_disable_hotkey.clone(),
            gestures: s.gestures.clone(),
            strip_scroll: ScrollGestureSettings {
                enabled: g.enabled,
                invert_horizontal: g.invert_horizontal,
                vertical_tolerance: g.vertical_tolerance,
                fingers: g.fingers,
                distance_pct: g.distance_pct,
            },
        }
    }
}

#[cfg(test)]
mod tests;
