//! The config file: schema, parsing with suggestions, validation, save. Defaults and the
//! documented shape live in `rini.default.toml`, which is embedded here.

use std::path::{Path, PathBuf};
use std::str::FromStr;

use anyhow::bail;
pub use rini_ipc::protocol::{ConfigCommand, WorkspaceSelector};
pub use rini_windows::rules::{AppRulePosition, AppRuleSize, AppWorkspaceRule};
pub use rini_displays::cursor_warp::StackedUpperSide;
pub use rini_workspaces::settings::{MAX_WORKSPACES, VirtualWorkspaceSettings};
pub use rini_tiling::settings::{
    BaseLayoutSettings, GapOverride, GapSettings, InnerGaps, LayoutSettings, OuterGaps,
    ScrollingAlignment, ScrollingFocusNavigationStyle, ScrollingGestureSettings,
    ScrollingLayoutSettings, WindowInsertionPoint,
};
use serde::{Deserialize, Serialize};

use rustc_hash::FxHashMap as HashMap;
use rini_input::key::{Hotkey, HotkeySpec};

pub mod actor;
pub mod watcher;

pub use rini_input::binding::{Command, ExecCmd, WmCmd, WmCommand};
pub use rini_input::haptics::HapticPattern;
pub use rini_input::settings::{GestureSettings, InputSettings, ScrollGestureSettings, WindowSnappingSettings};


pub fn data_dir() -> PathBuf {
    dirs::home_dir().unwrap().join(".rini")
}
pub fn restore_file() -> PathBuf {
    data_dir().join("layout.ron")
}
pub fn config_file() -> PathBuf {
    dirs::home_dir().unwrap().join(".config").join("rini").join("config.toml")
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


#[derive(Serialize, Deserialize, Debug, PartialEq, Clone)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    /// Whether layout changes, strip movements and workspace switches are animated. Everything
    /// animates through the overlay (`crates/rini-animation/docs/animation-smoothness.md`); off places windows at once.
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

        issues.extend(self.gestures.validate());

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



// Interpreted as normalized fraction when <= 1.0. If > 1.0 and <= 100.0,
// it is treated as a percentage (e.g. 40.0 -> 0.40).


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
                        rini_input::key::expand_modifier_combination(&key, &c.modifier_combinations);
                    let normalized_key = rini_input::key::normalize_spec(&expanded_key);
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
mod tests {
    use super::*;
    use rini_ipc::protocol::{LayoutCommand, ResizeOrientation};

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


    #[test]
    fn input_settings_are_assembled_from_both_the_input_and_the_tiling_tables() {
        let config: Config = Config::parse(
            r#"
                [settings]
                focus_follows_mouse = false
                mouse_hides_on_focus = false
                [settings.gestures]
                enabled = true
                fingers = 4
                [settings.layout.scrolling.gestures]
                enabled = true
                fingers = 2
                distance_pct = 0.2
                [keys]
            "#,
        )
        .unwrap();
        let input = InputSettings::from(&config);
        assert!(!input.focus_follows_mouse);
        assert!(!input.mouse_hides_on_focus);
        assert!(input.gestures.enabled);
        assert_eq!(input.gestures.fingers, 4);
        assert!(input.strip_scroll.enabled);
        assert_eq!(input.strip_scroll.fingers, 2);
        assert_eq!(input.strip_scroll.distance_pct, 0.2);
    }
}
