//! What the transitions can be told about themselves. Plain data; the config file fills it from
//! `animations › journal`.

use serde::Serialize;

/// The journal of every transition (`src/transitions/docs/journal.md`). Its ring is kept either way.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct JournalSettings {
    /// Each burst written once it settles.
    pub write: bool,
}

impl Default for JournalSettings {
    fn default() -> Self { JournalSettings { write: true } }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_journal_is_written_unless_turned_off() {
        assert!(JournalSettings::default().write);
    }
}
