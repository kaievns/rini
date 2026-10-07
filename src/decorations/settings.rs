//! What the config can say about the decorations rini draws: `layout › focus-ring` and
//! `layout › border`. Plain data.

use serde::Serialize;

use crate::decorations::domain::paint::Colour;
use crate::decorations::domain::style::{Band, Decoration};

/// A band the config turns on or off, and how it is drawn while on.
#[derive(Serialize, Debug, PartialEq, Clone, Copy)]
pub struct BandSetting {
    pub on: bool,
    pub band: Band,
}

impl BandSetting {
    fn worn(&self) -> Option<Band> { self.on.then_some(self.band) }
}

#[derive(Serialize, Debug, PartialEq, Clone, Copy)]
pub struct DecorationSettings {
    pub focus_ring: BandSetting,
    pub border: BandSetting,
}

impl Default for DecorationSettings {
    /// niri's focus ring, on, but 2pt wide where niri's is 4 (Kai's decision D1,
    /// `specs/decorations.md`); and niri's border, off.
    fn default() -> Self {
        Self {
            focus_ring: BandSetting {
                on: true,
                band: Band { width: 2.0, active: Colour::rgb(0x7fc8ff), inactive: Colour::rgb(0x505050) },
            },
            border: BandSetting {
                on: false,
                band: Band { width: 4.0, active: Colour::rgb(0xffc87f), inactive: Colour::rgb(0x505050) },
            },
        }
    }
}

impl DecorationSettings {
    /// What every managed window wears.
    pub fn decoration(&self) -> Decoration {
        Decoration { ring: self.focus_ring.worn(), border: self.border.worn() }
    }

    /// The width every tiled window gives up on each side to its border, 0 with borders off: the
    /// config's width as written, which the band draws in whole device pixels.
    pub fn border_width(&self) -> f64 { self.border.worn().map_or(0.0, |band| band.width) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_focus_ring_is_niris_and_on_unless_turned_off() {
        let settings = DecorationSettings::default();
        assert_eq!(settings.decoration().ring, Some(settings.focus_ring.band));
        let off = DecorationSettings { focus_ring: BandSetting { on: false, ..settings.focus_ring }, ..settings };
        assert_eq!(off.decoration(), Decoration::default());
    }

    #[test]
    fn the_border_is_niris_and_off_unless_turned_on() {
        let settings = DecorationSettings::default();
        assert_eq!(settings.decoration().border, None);
        assert_eq!(settings.border.band, Band { width: 4.0, active: Colour::rgb(0xffc87f), inactive: Colour::rgb(0x505050) });
        let on = DecorationSettings { border: BandSetting { on: true, ..settings.border }, ..settings };
        assert_eq!(on.decoration().border, Some(settings.border.band));
        assert_eq!((settings.border_width(), on.border_width()), (0.0, 4.0), "the slot gives up nothing while it is off");
    }
}
