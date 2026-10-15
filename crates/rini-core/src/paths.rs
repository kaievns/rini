//! Where rini keeps its files. The daemon reads and writes them; the transitions journal is read
//! after the fact by `tools/focuslab/journal.py`, which names the same path.

use std::path::PathBuf;

/// `~/.rini`: saved layout state, written by the daemon.
pub fn data_dir() -> PathBuf {
    dirs::home_dir().unwrap().join(".rini")
}

/// `~/.rini/layout.ron`: the layout snapshot rini saves as it runs and at quit, and restores at start.
pub fn restore_file() -> PathBuf {
    data_dir().join("layout.ron")
}

/// `~/.config/rini/config.kdl`: the user's config, watched for reload.
pub fn config_file() -> PathBuf {
    dirs::home_dir().unwrap().join(".config").join("rini").join("config.kdl")
}

/// `~/Library/Logs/rini/transitions.jsonl`: the transitions journal, one JSON object a line, written
/// by the daemon after each transition settles.
pub fn journal_file() -> PathBuf {
    dirs::home_dir().unwrap().join("Library").join("Logs").join("rini").join("transitions.jsonl")
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
        assert!(path.ends_with(".config/rini/config.kdl"), "{path:?}");
    }

    #[test]
    fn the_transitions_journal_is_kept_with_the_logs() {
        let path = journal_file();
        assert!(path.ends_with("Library/Logs/rini/transitions.jsonl"), "{path:?}");
    }
}
