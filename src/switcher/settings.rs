//! What the switcher can be told about itself. Plain data; the config file fills it from
//! `recent-windows`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, PartialEq, Clone, Default)]
pub struct SwitcherSettings {
    /// An image to badge a tile with in place of its application's own icon, by bundle identifier.
    ///
    /// For applications that set their icon while running, which only the Dock can see: Ghostty's
    /// `macos-icon`, for one, is drawn at launch and handed to the Dock alone, so every API another
    /// process can call still returns the icon in the bundle. A path starting `~/` is under the home
    /// directory.
    pub icons: BTreeMap<String, String>,
}

impl SwitcherSettings {
    /// The image configured for `bundle_id`, with a leading `~/` resolved against `home`.
    ///
    /// Only a path: whether the file exists is found out when it is drawn, and a missing one falls back
    /// to the application's own icon rather than rejecting the whole config over a picture.
    pub fn icon_for(&self, bundle_id: Option<&str>, home: Option<&Path>) -> Option<PathBuf> {
        let spec = self.icons.get(bundle_id?)?;
        match (spec.strip_prefix("~/"), home) {
            (Some(rest), Some(home)) => Some(home.join(rest)),
            _ => Some(PathBuf::from(spec)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(entries: &[(&str, &str)]) -> SwitcherSettings {
        SwitcherSettings {
            icons: entries.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
        }
    }

    #[test]
    fn a_configured_application_gets_its_image() {
        let s = settings(&[("com.mitchellh.ghostty", "/icons/ghostty.png")]);
        assert_eq!(
            s.icon_for(Some("com.mitchellh.ghostty"), None),
            Some(PathBuf::from("/icons/ghostty.png"))
        );
    }

    #[test]
    fn a_home_relative_path_is_resolved() {
        let s = settings(&[("com.mitchellh.ghostty", "~/.config/rini/icons/ghostty.png")]);
        assert_eq!(
            s.icon_for(Some("com.mitchellh.ghostty"), Some(Path::new("/Users/k"))),
            Some(PathBuf::from("/Users/k/.config/rini/icons/ghostty.png"))
        );
    }

    /// Everything else keeps its own icon, including an application with no bundle identifier.
    #[test]
    fn an_application_not_listed_keeps_its_own_icon() {
        let s = settings(&[("com.mitchellh.ghostty", "/icons/ghostty.png")]);
        assert_eq!(s.icon_for(Some("com.google.Chrome"), None), None);
        assert_eq!(s.icon_for(None, None), None);
    }
}
