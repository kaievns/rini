//! What the config can say about the decorations rini draws: `layout › focus-ring`,
//! `layout › border`, and what each `window-rule` says of them. Plain data.

use serde::Serialize;

use crate::decorations::domain::paint::{Colour, Paint};
use crate::decorations::domain::rules::DecorationRule;
use crate::decorations::domain::style::Band;

/// A band the config turns on or off, and how it is drawn while on.
#[derive(Serialize, Debug, PartialEq, Clone, Copy)]
pub struct BandSetting {
    pub on: bool,
    pub band: Band,
}

impl BandSetting {
    /// The band, while it is on.
    pub fn worn(&self) -> Option<Band> { self.on.then_some(self.band) }
}

#[derive(Serialize, Debug, PartialEq, Clone)]
pub struct DecorationSettings {
    pub focus_ring: BandSetting,
    pub border: BandSetting,
    /// What each `window-rule` says a window wears, in file order (`domain::rules::fold`).
    pub rules: Vec<DecorationRule>,
}

impl Default for DecorationSettings {
    /// niri's focus ring, on, but 2pt wide where niri's is 4 (Kai's decision D1,
    /// `specs/decorations.md`); and niri's border, off. Both urgent in niri's `#9b0000`.
    fn default() -> Self {
        Self {
            focus_ring: BandSetting {
                on: true,
                band: Band {
                    width: 2.0,
                    active: Paint::Solid(Colour::rgb(0x7fc8ff)),
                    inactive: Paint::Solid(Colour::rgb(0x505050)),
                    urgent: Paint::Solid(Colour::rgb(0x9b0000)),
                },
            },
            border: BandSetting {
                on: false,
                band: Band {
                    width: 4.0,
                    active: Paint::Solid(Colour::rgb(0xffc87f)),
                    inactive: Paint::Solid(Colour::rgb(0x505050)),
                    urgent: Paint::Solid(Colour::rgb(0x9b0000)),
                },
            },
            rules: Vec::new(),
        }
    }
}

impl DecorationSettings {
    /// Whether any window can wear a band: the ring or the border is on, or a rule turns one on.
    pub fn draws_bands(&self) -> bool {
        self.focus_ring.on
            || self.border.on
            || self.rules.iter().any(|rule| rule.focus_ring.on == Some(true) || rule.border.on == Some(true))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decorations::domain::rules::{BandRule, fold};
    use crate::decorations::domain::style::Decoration;
    use crate::windows::domain::rules::WindowRuleContext;

    fn worn(settings: &DecorationSettings) -> Decoration { fold(settings, WindowRuleContext::default()) }

    #[test]
    fn the_focus_ring_is_niris_and_on_unless_turned_off() {
        let settings = DecorationSettings::default();
        assert_eq!(worn(&settings).ring, Some(settings.focus_ring.band));
        let off = DecorationSettings { focus_ring: BandSetting { on: false, ..settings.focus_ring }, ..settings.clone() };
        assert_eq!(worn(&off), Decoration::default());
        assert!(settings.draws_bands() && !off.draws_bands());
    }

    #[test]
    fn the_border_is_niris_and_off_unless_turned_on() {
        let settings = DecorationSettings::default();
        assert_eq!(worn(&settings).border, None);
        let solid = |rgb| Paint::Solid(Colour::rgb(rgb));
        assert_eq!(settings.border.band, Band { width: 4.0, active: solid(0xffc87f), inactive: solid(0x505050), urgent: solid(0x9b0000) });
        let on = DecorationSettings { border: BandSetting { on: true, ..settings.border }, ..settings.clone() };
        assert_eq!(worn(&on).border, Some(settings.border.band));
    }

    #[test]
    fn a_rule_turning_a_band_on_draws_bands_with_both_off() {
        let off = BandSetting { on: false, ..DecorationSettings::default().focus_ring };
        let rule = DecorationRule { border: BandRule { on: Some(true), ..BandRule::default() }, ..DecorationRule::default() };
        let settings = DecorationSettings { focus_ring: off, rules: vec![rule], ..DecorationSettings::default() };
        assert!(settings.draws_bands());
        assert!(!DecorationSettings { rules: Vec::new(), ..settings }.draws_bands());
    }
}
