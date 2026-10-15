//! What the transitions can be told about themselves. Plain data; the config file fills it from
//! `animations › journal`.

use serde::Serialize;

/// The journal of every transition (`src/transitions/docs/journal.md`). Its ring is kept either way.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct JournalSettings {
    /// Each burst written once it settles.
    pub write: bool,
    /// Each lift held for the overlay's own picture and measured against the screen after it.
    pub measure_handover: bool,
}

impl Default for JournalSettings {
    fn default() -> Self {
        JournalSettings {
            write: false,
            measure_handover: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_journal_is_written_only_when_asked() {
        assert!(!JournalSettings::default().write);
    }

    #[test]
    fn the_handover_is_measured_only_when_asked() {
        assert!(!JournalSettings::default().measure_handover);
    }
}
