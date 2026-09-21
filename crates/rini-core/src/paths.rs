//! Where rini keeps its files. The daemon writes them and the CLI reads them, so the answer cannot
//! live in either.

use std::path::PathBuf;

/// `~/.rini`: saved layout state, written by the daemon.
pub fn data_dir() -> PathBuf {
    dirs::home_dir().unwrap().join(".rini")
}

/// `~/.rini/layout.ron`: the layout snapshot `--restore` reads and `save-layout --saved` writes.
pub fn restore_file() -> PathBuf {
    data_dir().join("layout.ron")
}

/// `~/.config/rini/config.toml`: the user's config, watched for reload.
pub fn config_file() -> PathBuf {
    dirs::home_dir().unwrap().join(".config").join("rini").join("config.toml")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_layout_snapshot_sits_in_the_data_dir() {
        assert_eq!(restore_file().parent().unwrap(), data_dir());
        assert_eq!(restore_file().file_name().unwrap(), "layout.ron");
    }

    #[test]
    fn the_config_lives_under_dot_config_rini() {
        let path = config_file();
        assert!(path.ends_with(".config/rini/config.toml"), "{path:?}");
    }
}
