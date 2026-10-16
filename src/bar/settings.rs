//! What the bar can be told about itself. Plain data; the config file fills it from `bar`.

#[derive(Debug, PartialEq, Clone)]
pub struct BarSettings {
    /// Draw rini's bar. Off, no bar is drawn and windows get the band back, for running another bar
    /// in its place.
    pub enabled: bool,
}

impl Default for BarSettings {
    fn default() -> Self { Self { enabled: true } }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_bar_is_on_unless_turned_off() {
        assert!(BarSettings::default().enabled);
    }
}
