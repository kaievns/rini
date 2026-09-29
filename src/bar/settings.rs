//! What the bar can be told about itself. Plain data; the config file fills it under
//! `[settings.bar]`.

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, PartialEq, Clone)]
#[serde(deny_unknown_fields)]
pub struct BarSettings {
    /// Draw rini's bar. Off, no bar is drawn and windows get the band back, for running another bar
    /// in its place.
    #[serde(default = "on")]
    pub enabled: bool,
}

impl Default for BarSettings {
    fn default() -> Self {
        Self { enabled: on() }
    }
}

fn on() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_bar_is_on_unless_turned_off() {
        assert!(BarSettings::default().enabled);
        let parsed: BarSettings = toml::from_str("").expect("parses");
        assert_eq!(parsed, BarSettings::default());
    }

    #[test]
    fn it_can_be_turned_off() {
        let parsed: BarSettings = toml::from_str("enabled = false").expect("parses");
        assert!(!parsed.enabled);
    }

    #[test]
    fn a_misspelt_key_is_an_error() {
        assert!(toml::from_str::<BarSettings>("enable = false").is_err());
    }
}
