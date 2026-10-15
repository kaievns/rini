//! What the flags and the config file say, decided before anything is built.
//!
//! `main` is one long function by nature — it assembles eleven actors and hands them to each other —
//! but the DECISIONS in it are not, so they live here, where a test reaches them without running
//! the binary.

use std::path::{Path, PathBuf};

use crate::app::config::{Config, Settings};

/// The config, or the built-in defaults and the reason the file could not be used.
///
/// A broken config MUST NOT stop rini starting: with no window manager running there is no way to
/// open an editor except from a terminal that happens to be open already. On the defaults rini keeps
/// its binds and re-reads the file when it is saved, so the file can be fixed in place.
///
/// A missing file means the defaults, unless the TOML config sits beside it (`leftover_toml`).
pub fn config_or_default(path: &Path) -> (Config, Option<String>) {
    if !path.exists() {
        return (Config::default(), leftover_toml(path));
    }
    match Config::read(path) {
        Ok(config) => (config, None),
        Err(error) => (
            Config::default(),
            Some(format!(
                "Could not use the config at {}; starting on the built-in defaults, \
                 rini.default.kdl. Fix the config and it is read again as soon as it is saved.\n{}",
                path.display(),
                error.render()
            )),
        ),
    }
}

/// Why a missing `path` is not simply the defaults: a `config.toml` beside it is a config that was
/// written and is no longer read.
fn leftover_toml(path: &Path) -> Option<String> {
    let toml = path.with_file_name("config.toml");
    toml.exists().then(|| {
        format!(
            "No config at {}, but {} is there: rini's config is KDL, and until {} exists rini runs \
             on its built-in defaults, rini.default.kdl. docs/config.md has the format.",
            path.display(),
            toml.display(),
            path.display()
        )
    })
}

/// What `rini --validate` says about the config: a line naming it when it is good, the report when
/// it is not. A missing file is good, since it means the defaults, unless `leftover_toml` says why
/// not.
pub fn validate_config(path: &Path) -> Result<String, String> {
    if !path.exists() {
        return match leftover_toml(path) {
            Some(complaint) => Err(complaint),
            None => Ok(format!("No config at {}; rini uses its built-in defaults", path.display())),
        };
    }
    Config::read(path)
        .map(|_| format!("config is valid: {}", path.display()))
        .map_err(|error| error.render())
}

/// Where the config is, given whatever `--config` said.
///
/// A named path is taken as given, including one that does not exist: `config_or_default` reports a
/// missing file as "use the defaults", and second-guessing the user's path here would hide a typo in
/// it behind a silent fallback to their real config.
pub fn config_path(explicit: Option<PathBuf>) -> PathBuf {
    explicit.unwrap_or_else(rini_core::paths::config_file)
}

/// Apply the two command-line flags that override the config.
///
/// Both are one-directional, and deliberately so. `--no-animate` can only turn animation OFF and
/// `--default-disable` can only turn the disabled start ON; neither can undo what the config says in
/// the other direction, because there is no `--animate` or `--no-default-disable` to do that with.
///
/// So the flags are for a one-off run that differs from the config — starting without animation to
/// see whether animation is what is wrong — rather than a second place to configure rini.
pub fn apply_flag_overrides(settings: &mut Settings, no_animate: bool, default_disable: bool) {
    settings.animate &= !no_animate;
    settings.default_disable |= default_disable;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_config(name: &str, text: &str) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(name);
        std::fs::write(&path, text).unwrap();
        (dir, path)
    }

    #[test]
    fn a_missing_config_file_is_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let (_, complaint) = config_or_default(&dir.path().join("config.kdl"));
        assert_eq!(
            complaint, None,
            "no file means the defaults, which is not a failure"
        );
    }

    /// The defaults run every Space inactive, so starting on them silently in place of a TOML config
    /// looks like rini is broken.
    #[test]
    fn a_missing_config_beside_a_toml_one_is_a_config_that_cannot_be_used() {
        let (dir, toml) = temp_config("config.toml", "[settings]\nanimate = false\n");
        let kdl = dir.path().join("config.kdl");

        let (config, complaint) = config_or_default(&kdl);

        let complaint = complaint.expect("the TOML file is named, not ignored");
        for named in [kdl.display().to_string(), toml.display().to_string()] {
            assert!(complaint.contains(&named), "{named} missing: {complaint}");
        }
        assert!(complaint.contains("docs/config.md"), "{complaint}");
        assert_eq!(config.settings, Config::default().settings);

        let report = validate_config(&kdl).unwrap_err();
        assert!(report.contains("config.toml"), "{report}");
    }

    /// The history this exists for: one mistyped keybinding used to take the window manager down at
    /// launch, and with nothing managing windows there is no way to open an editor to fix it.
    #[test]
    fn an_unparseable_config_falls_back_to_the_defaults_and_says_so() {
        let (_dir, path) = temp_config("config.kdl", "animations {\n    off \"not a bool\"\n}\n");

        let (config, complaint) = config_or_default(&path);

        let complaint = complaint.expect("a refused config has to say why");
        assert!(complaint.contains("built-in defaults"), "{complaint}");
        assert!(complaint.contains("config.kdl:2:"), "the report names the line: {complaint}");
        assert_eq!(
            config.settings.animate,
            Config::default().settings.animate,
            "the defaults, not a half-read file"
        );
    }

    /// A binding naming an action that does not exist is the exact shape of the failure: renaming an
    /// action invalidates a config that used it, and that must not be fatal.
    #[test]
    fn a_config_naming_an_unknown_action_falls_back_rather_than_failing() {
        let (_dir, path) = temp_config("config.kdl", "binds {\n    Alt+Q { no-such-action; }\n}\n");

        let (_, complaint) = config_or_default(&path);

        assert!(
            complaint.is_some_and(|complaint| complaint.contains("unknown action `no-such-action`")),
            "an unknown action is a refusal, not a silent ignore"
        );
    }

    /// Every section is optional: a file with only one setting in it is a whole config.
    #[test]
    fn a_valid_config_produces_no_complaint() {
        let (_dir, path) = temp_config("config.kdl", "animations {\n    off\n}\n");

        let (config, complaint) = config_or_default(&path);

        assert_eq!(complaint, None);
        assert!(!config.settings.animate);
    }

    #[test]
    fn validate_names_a_good_config_and_reports_a_bad_one_with_its_line() {
        let (_dir, good) = temp_config("good.kdl", "layout {\n    gaps 4\n}\n");
        assert!(validate_config(&good).unwrap().starts_with("config is valid: "));

        let (_dir, bad) = temp_config("bad.kdl", "layout {\n    gasp 4\n}\n");
        let report = validate_config(&bad).unwrap_err();
        assert!(report.contains("bad.kdl:2:5"), "{report}");
        assert!(report.contains("did you mean `gaps`?"), "{report}");

        let missing = good.with_file_name("absent.kdl");
        assert!(validate_config(&missing).unwrap().contains("built-in defaults"));
    }

    #[test]
    fn a_named_config_path_is_taken_as_given() {
        let named = PathBuf::from("/tmp/somewhere/rini.kdl");
        assert_eq!(config_path(Some(named.clone())), named);
    }

    #[test]
    fn no_named_path_means_the_default_location() {
        assert_eq!(config_path(None), rini_core::paths::config_file());
    }

    /// A path that does not exist is still the path: `config_or_default` reports a missing file as
    /// "use the defaults", and falling back to the real config here would hide a typo in `--config`
    /// behind the user's actual settings.
    #[test]
    fn a_named_path_that_does_not_exist_is_not_second_guessed() {
        let absent = PathBuf::from("/tmp/rini-does-not-exist-9e7c.kdl");
        assert_eq!(config_path(Some(absent.clone())), absent);
        assert_ne!(config_path(Some(absent)), rini_core::paths::config_file());
    }

    fn settings(animate: bool, default_disable: bool) -> Settings {
        let mut settings = Config::default().settings;
        settings.animate = animate;
        settings.default_disable = default_disable;
        settings
    }

    #[test]
    fn no_flags_leave_the_config_alone() {
        let mut on = settings(true, false);
        apply_flag_overrides(&mut on, false, false);
        assert!(on.animate);
        assert!(!on.default_disable);
    }

    #[test]
    fn the_flags_override_the_config_in_their_own_direction() {
        let mut settings = settings(true, false);
        apply_flag_overrides(&mut settings, true, true);
        assert!(!settings.animate, "--no-animate turns animation off");
        assert!(settings.default_disable, "--default-disable starts disabled");
    }

    /// The asymmetry, stated. Each flag pushes one way only, because there is no `--animate` or
    /// `--no-default-disable` to push back with, so the flags are for a one-off run that differs from
    /// the config rather than a second place to configure rini.
    #[test]
    fn neither_flag_can_undo_the_config_in_the_other_direction() {
        let mut animation_off_in_config = settings(false, true);
        apply_flag_overrides(&mut animation_off_in_config, false, false);
        assert!(
            !animation_off_in_config.animate,
            "no flag turns animation back on"
        );
        assert!(
            animation_off_in_config.default_disable,
            "no flag undoes a disabled start"
        );
    }

    #[test]
    fn a_flag_applied_twice_says_the_same_thing() {
        let mut once = settings(true, false);
        apply_flag_overrides(&mut once, true, true);
        let mut twice = once.clone();
        apply_flag_overrides(&mut twice, true, true);
        assert_eq!(twice.animate, once.animate);
        assert_eq!(twice.default_disable, once.default_disable);
    }
}
