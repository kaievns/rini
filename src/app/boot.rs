//! What the flags and the config file say, decided before anything is built.
//!
//! `main` is one long function by nature — it assembles nine actors and hands them to each other —
//! but the two DECISIONS in it are not, and both have a recorded history behind them. They were
//! reachable only by running the binary, so neither was tested.

use std::path::{Path, PathBuf};

use crate::app::config::{Config, Settings};

/// Whether to restore the saved layout.
///
/// On by default, so a restart or a redeploy keeps window sizes, workspaces and strip positions.
/// `--restore` is accepted for compatibility and as an explicit override, so giving BOTH flags
/// restores: the positive one wins. Reading that off `!no_restore || restore` is not obvious, which
/// is the reason it is a named function with a test.
pub fn wants_restore(restore: bool, no_restore: bool) -> bool {
    !no_restore || restore
}

/// The config, or the built-in defaults and the reason the file could not be used.
///
/// A broken config MUST NOT stop rini starting. This used to be an unwrap, so one unparseable line
/// took the whole window manager down at launch — and with no window manager running there is no
/// way to open an editor except from a terminal that happens to be open already. One mistyped
/// keybinding was enough.
///
/// Falling back keeps windows managed and the hotkeys for editing the config reachable, which is the
/// only state a user can actually recover from. A missing file is not an error: it means the defaults.
pub fn config_or_default(path: &Path) -> (Config, Option<String>) {
    if !path.exists() {
        return (Config::default(), None);
    }
    match Config::read(path) {
        Ok(config) => (config, None),
        Err(error) => (
            Config::default(),
            Some(format!(
                "Could not read the config at {}; starting with the built-in defaults so rini \
                 stays usable. Fix the config and restart. Error: {error}",
                path.display()
            )),
        ),
    }
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

    #[test]
    fn restore_is_on_by_default() {
        assert!(wants_restore(false, false));
    }

    #[test]
    fn no_restore_turns_it_off() {
        assert!(!wants_restore(false, true));
    }

    /// Both flags together restore. `--restore` exists as an explicit override, so the positive one
    /// wins; the alternative is that a script passing both silently starts from a clean layout.
    #[test]
    fn restore_wins_when_both_flags_are_given() {
        assert!(wants_restore(true, true));
    }

    #[test]
    fn a_missing_config_file_is_not_an_error() {
        let path = std::env::temp_dir().join(format!("rini-absent-{}.toml", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let (_, complaint) = config_or_default(&path);
        assert_eq!(
            complaint, None,
            "no file means the defaults, which is not a failure"
        );
    }

    /// The history this exists for: one mistyped keybinding used to take the window manager down at
    /// launch, and with nothing managing windows there is no way to open an editor to fix it.
    #[test]
    fn an_unparseable_config_falls_back_to_the_defaults_and_says_so() {
        let path = std::env::temp_dir().join(format!("rini-broken-{}.toml", std::process::id()));
        std::fs::write(&path, "[settings]\nanimate = \"not a bool\"\n\n[keys]\n").unwrap();

        let (config, complaint) = config_or_default(&path);
        let _ = std::fs::remove_file(&path);

        let complaint = complaint.expect("a refused config has to say why");
        assert!(complaint.contains("built-in defaults"), "{complaint}");
        assert!(complaint.contains("Fix the config and restart"), "{complaint}");
        assert_eq!(
            config.settings.animate,
            Config::default().settings.animate,
            "the defaults, not a half-read file"
        );
    }

    /// A binding naming a command that does not exist is the exact shape of the failure: deleting a
    /// command from the protocol invalidates a config that used it, and that must not be fatal.
    #[test]
    fn a_config_naming_an_unknown_command_falls_back_rather_than_failing() {
        let path = std::env::temp_dir().join(format!("rini-unknown-{}.toml", std::process::id()));
        std::fs::write(&path, "[keys]\n\"Alt + Q\" = \"no_such_command\"\n").unwrap();

        let (_, complaint) = config_or_default(&path);
        let _ = std::fs::remove_file(&path);

        assert!(
            complaint.is_some(),
            "an unknown command is a refusal, not a silent ignore"
        );
    }

    #[test]
    fn a_valid_config_produces_no_complaint() {
        let path = std::env::temp_dir().join(format!("rini-ok-{}.toml", std::process::id()));
        // `keys` is a TOP-LEVEL table and is required: the binding table has no default, because a
        // window manager with no bindings is not a state anyone should reach by omission.
        std::fs::write(&path, "[settings]\nanimate = false\n\n[keys]\n").unwrap();

        let (config, complaint) = config_or_default(&path);
        let _ = std::fs::remove_file(&path);

        assert_eq!(complaint, None);
        assert!(!config.settings.animate);
    }

    #[test]
    fn a_named_config_path_is_taken_as_given() {
        let named = PathBuf::from("/tmp/somewhere/rini.toml");
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
        let absent = PathBuf::from("/tmp/rini-does-not-exist-9e7c.toml");
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
