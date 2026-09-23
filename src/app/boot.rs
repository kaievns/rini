//! What the flags and the config file say, decided before anything is built.
//!
//! `main` is one long function by nature — it assembles nine actors and hands them to each other —
//! but the two DECISIONS in it are not, and both have a recorded history behind them. They were
//! reachable only by running the binary, so neither was tested.

use std::path::Path;

use crate::app::config::Config;

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
}
