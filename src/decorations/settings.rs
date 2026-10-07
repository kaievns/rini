//! What the config can say about the decorations rini draws: `layout › focus-ring`. Plain data.

use serde::Serialize;

use crate::decorations::domain::paint::Colour;
use crate::decorations::domain::style::{Band, Decoration};

/// A band the config turns on or off, and how it is drawn while on.
#[derive(Serialize, Debug, PartialEq, Clone, Copy)]
pub struct BandSetting {
    pub on: bool,
    pub band: Band,
}

#[derive(Serialize, Debug, PartialEq, Clone, Copy)]
pub struct DecorationSettings {
    pub focus_ring: BandSetting,
}

impl Default for DecorationSettings {
    /// niri's focus ring, on (Kai's decision D1, `specs/decorations.md`).
    fn default() -> Self {
        Self {
            focus_ring: BandSetting {
                on: true,
                band: Band { width: 4.0, active: Colour::rgb(0x7fc8ff), inactive: Colour::rgb(0x505050) },
            },
        }
    }
}

impl DecorationSettings {
    /// What every managed window wears.
    pub fn decoration(&self) -> Decoration {
        Decoration { ring: self.focus_ring.on.then_some(self.focus_ring.band) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_focus_ring_is_niris_and_on_unless_turned_off() {
        let settings = DecorationSettings::default();
        assert_eq!(settings.decoration().ring, Some(settings.focus_ring.band));
        let off = DecorationSettings { focus_ring: BandSetting { on: false, ..settings.focus_ring } };
        assert_eq!(off.decoration(), Decoration::default());
    }
}
