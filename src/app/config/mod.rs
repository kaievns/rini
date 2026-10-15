//! The config: what rini runs on, read from `config.kdl`. Its defaults are the features' own
//! `Default` impls; `rini.default.kdl`, embedded here, documents them and is used whenever the file
//! is missing or cannot be used. The format and its decisions are in `docs/config.md`.

use std::path::Path;

pub use rini_ipc::protocol::ConfigCommand;
use serde::Serialize;

use crate::app::startup::StartupCommand;
use crate::displays::platform::cursor_warp::StackedUpperSide;
use crate::input::domain::binding::Bind;
use crate::input::domain::key::ModKey;
use crate::input::settings::{InputSettings, WindowSnappingSettings};
pub use crate::layout::settings::LayoutSettings;
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
    pub activate_spaces_by_default: bool,
    pub warp_mouse_to_focus: bool,
    pub warp_cursor_between_stacked_displays: bool,
    pub stacked_display_upper_is: StackedUpperSide,
    /// Fraction up the upper display's height at which the lower display's top edge sits.
    pub stacked_display_lower_top_at: f64,
    pub focus_follows_mouse: bool,
    /// Bundle ids whose activation must not switch workspaces (Spotlight-style focus stealers).
    pub ignore_app_activation: Vec<String>,
    /// The modifier `Mod` stands for in a key.
    pub mod_key: ModKey,
    pub layout: LayoutSettings,
    pub switcher: crate::switcher::settings::SwitcherSettings,
    pub bar: crate::bar::settings::BarSettings,
    /// `animations › journal`: whether every transition's journal is written, and each lift's handover
    /// measured, which holds a lift's hide for the overlay's own picture.
    pub journal: crate::transitions::settings::JournalSettings,
    pub window_snapping: WindowSnappingSettings,
    /// `layout › focus-ring`, `layout › border` and what each `window-rule` says of them: the ring
    /// and the border rini draws around windows.
    pub decorations: crate::decorations::settings::DecorationSettings,
    pub run_on_start: Vec<StartupCommand>,
    /// Mark every bar while the config file cannot be used, until it can.
    pub mark_failed_config: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            animate: true,
            animation_duration: BASE_ANIMATION_SECONDS,
            activate_spaces_by_default: false,
            warp_mouse_to_focus: false,
            warp_cursor_between_stacked_displays: false,
            stacked_display_upper_is: StackedUpperSide::default(),
            // The laptop's top edge a little under halfway up the larger screen, which is where a
            // laptop parked beside a big monitor tends to sit.
            stacked_display_lower_top_at: 0.4,
            focus_follows_mouse: false,
            ignore_app_activation: Vec::new(),
            mod_key: ModKey::default(),
            layout: LayoutSettings::default(),
            switcher: Default::default(),
            bar: Default::default(),
            journal: Default::default(),
            window_snapping: WindowSnappingSettings::default(),
            decorations: Default::default(),
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
    /// The embedded `rini.default.kdl`. A test parses it by name, so a broken one fails there first.
    pub fn reference() -> Config {
        Self::parse("rini.default.kdl", DEFAULT_KDL)
            .unwrap_or_else(|error| panic!("{}", error.render()))
    }

    /// What every setting is when nothing says otherwise: no binds, no window rules.
    pub fn code_defaults() -> Config {
        Config {
            settings: Settings::default(),
            binds: Vec::new(),
            switcher_binds: Vec::new(),
            virtual_workspaces: VirtualWorkspaceSettings::default(),
        }
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
        InputSettings {
            focus_follows_mouse: s.focus_follows_mouse,
        }
    }
}

#[cfg(test)]
mod tests;
