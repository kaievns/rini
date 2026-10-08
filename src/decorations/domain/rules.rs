//! Window rules for what a window wears: a `window-rule`'s `focus-ring`, `border` and
//! `geometry-corner-radius`, folded over the layout's settings for one window as it is now. They
//! match through the matcher the open-time rules use (`windows::domain::rules::applies`) and apply
//! continuously, as niri's do: each window is folded afresh whenever what it wears is asked for.

use serde::Serialize;

use crate::decorations::domain::paint::Paint;
use crate::decorations::domain::style::{Band, Decoration};
use crate::decorations::settings::{BandSetting, DecorationSettings};
use crate::windows::domain::rules::{RuleMatch, WindowRuleContext, RuleState, applies};

/// What one rule says of a band: each part it leaves out keeps what came before it. A paint it
/// writes, colour or gradient, replaces the one before whole, so a colour clears a gradient, as
/// niri's `merge_color_gradient` has it (`niri:niri-config/src/macros.rs:31-57`).
#[derive(Serialize, Debug, PartialEq, Clone, Copy, Default)]
pub struct BandRule {
    pub on: Option<bool>,
    pub width: Option<f64>,
    pub active: Option<Paint>,
    pub inactive: Option<Paint>,
}

impl BandRule {
    /// `setting` with every part this rule writes replaced, so a later `off` beats an earlier `on`
    /// and the reverse (`niri:niri-config/src/macros.rs:59-71`).
    fn over(&self, setting: &mut BandSetting) {
        setting.on = self.on.unwrap_or(setting.on);
        setting.band.width = self.width.unwrap_or(setting.band.width);
        setting.band.active = self.active.unwrap_or(setting.band.active);
        setting.band.inactive = self.inactive.unwrap_or(setting.band.inactive);
    }

    /// Whether it says whether the band is drawn or how wide, rather than only how it is painted.
    pub fn shapes(&self) -> bool { self.on.is_some() || self.width.is_some() }
}

/// The part of a `window-rule` that says what a window wears.
#[derive(Serialize, Debug, PartialEq, Clone, Default)]
pub struct DecorationRule {
    pub matches: Vec<RuleMatch>,
    pub excludes: Vec<RuleMatch>,
    pub focus_ring: BandRule,
    pub border: BandRule,
    /// `geometry-corner-radius`, in points.
    pub corner_radius: Option<f64>,
}

impl DecorationRule {
    /// Whether it says anything of what a window wears.
    pub fn decorates(&self) -> bool {
        *self != DecorationRule { matches: self.matches.clone(), excludes: self.excludes.clone(), ..DecorationRule::default() }
    }

    /// Whether it applies to a window by whether the window holds focus.
    pub fn follows_focus(&self) -> bool { self.matches.iter().chain(&self.excludes).any(RuleMatch::follows_focus) }
}

/// What `context`'s window wears in the state `context` gives it: the layout's bands with every rule
/// that applies laid over them in file order.
pub fn fold(settings: &DecorationSettings, context: WindowRuleContext<'_>) -> Decoration {
    let (mut ring, mut border, mut radius) = (settings.focus_ring, settings.border, None);
    for rule in settings.rules.iter().filter(|rule| applies(&rule.matches, &rule.excludes, context)) {
        rule.focus_ring.over(&mut ring);
        rule.border.over(&mut border);
        radius = rule.corner_radius.or(radius);
    }
    Decoration { ring: ring.worn(), border: border.worn(), radius }
}

/// What `context`'s window wears whether it holds focus or not: each band's active paint as the
/// rules give it while the window holds focus, and everything else as they give it while it does not.
/// A rule that follows focus sets paints alone (the config refuses the rest), so this one value is
/// painted right on either side of a focus turn (`Style::of`).
pub fn decoration(settings: &DecorationSettings, context: WindowRuleContext<'_>) -> Decoration {
    let at = |focused| fold(settings, WindowRuleContext { state: RuleState { focused, ..context.state }, ..context });
    let (resting, holding) = (at(false), at(true));
    let active = |band: Option<Band>, held: Option<Band>| {
        band.map(|band| Band { active: held.map_or(band.active, |held| held.active), ..band })
    };
    Decoration {
        ring: active(resting.ring, holding.ring),
        border: active(resting.border, holding.border),
        radius: resting.radius,
    }
}

/// The width `context`'s window gives its border out of its slot while it is tiled, 0 while it wears
/// none. No rule that follows focus sets it, so it holds as focus moves.
pub fn border_width(settings: &DecorationSettings, context: WindowRuleContext<'_>) -> f64 {
    let tiled = WindowRuleContext { state: RuleState { focused: false, floating: false }, ..context };
    fold(settings, tiled).border.map_or(0.0, |band| band.width)
}

/// Whether any rule can give a window a border width other than the layout's.
pub fn shape_borders(settings: &DecorationSettings) -> bool { settings.rules.iter().any(|rule| rule.border.shapes()) }

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decorations::domain::paint::{Colour, ColourSpace, Gradient, Hue};
    use crate::windows::domain::rules::WindowRule;

    const RED: Paint = Paint::Solid(Colour::rgb(0xff0000));
    const GREEN: Paint = Paint::Solid(Colour::rgb(0x00ff00));

    fn app(bundle_id: &str) -> RuleMatch { RuleMatch::app(bundle_id) }

    fn window(bundle_id: &str) -> WindowRuleContext<'_> {
        WindowRuleContext { app_bundle_id: Some(bundle_id), ..WindowRuleContext::default() }
    }

    fn rule(line: RuleMatch) -> DecorationRule { DecorationRule { matches: vec![line], ..DecorationRule::default() } }

    fn with(rules: Vec<DecorationRule>) -> DecorationSettings { DecorationSettings { rules, ..DecorationSettings::default() } }

    #[test]
    fn a_later_rule_overrides_an_earlier_one() {
        let settings = with(vec![
            DecorationRule { focus_ring: BandRule { width: Some(6.0), active: Some(RED), ..BandRule::default() }, ..rule(app("com.example.Editor")) },
            DecorationRule { focus_ring: BandRule { active: Some(GREEN), ..BandRule::default() }, ..DecorationRule::default() },
        ]);
        let ring = fold(&settings, window("com.example.Editor")).ring.expect("on, as the layout has it");
        assert_eq!((ring.width, ring.active), (6.0, GREEN), "the width the first left, the colour the second wrote");
        assert_eq!(ring.inactive, settings.focus_ring.band.inactive, "what neither wrote is the layout's");
        let other = fold(&settings, window("com.example.Other")).ring.expect("on");
        assert_eq!((other.width, other.active), (settings.focus_ring.band.width, GREEN));
    }

    #[test]
    fn a_colour_in_a_later_part_clears_an_earlier_gradient() {
        let sunset = Paint::Gradient(Gradient {
            from: Colour::rgb(0xf38ba8),
            to: Colour::rgb(0xf9e2af),
            angle: 45.0,
            space: ColourSpace::Oklch(Hue::Longer),
        });
        let mut layout = DecorationSettings::default();
        layout.focus_ring.band.active = sunset;
        let settings = DecorationSettings {
            rules: vec![DecorationRule { focus_ring: BandRule { active: Some(RED), ..BandRule::default() }, ..rule(app("com.example.Editor")) }],
            ..layout
        };
        assert_eq!(fold(&settings, window("com.example.Editor")).ring.map(|band| band.active), Some(RED));
        assert_eq!(fold(&settings, window("com.example.Other")).ring.map(|band| band.active), Some(sunset), "the layout's gradient elsewhere");
        let regraded = DecorationRule { focus_ring: BandRule { active: Some(sunset), ..BandRule::default() }, ..DecorationRule::default() };
        let again = DecorationSettings { rules: vec![settings.rules[0].clone(), regraded], ..settings };
        assert_eq!(fold(&again, window("com.example.Editor")).ring.map(|band| band.active), Some(sunset), "and a later gradient the colour");
    }

    #[test]
    fn a_later_off_beats_an_earlier_on_across_rules() {
        let on = DecorationRule { border: BandRule { on: Some(true), ..BandRule::default() }, ..DecorationRule::default() };
        let off = DecorationRule { border: BandRule { on: Some(false), ..BandRule::default() }, ..rule(app("com.example.Editor")) };
        let settings = with(vec![on.clone(), off.clone()]);
        assert_eq!(fold(&settings, window("com.example.Editor")).border, None);
        assert_eq!(fold(&settings, window("com.example.Other")).border, Some(settings.border.band), "the first rule turned it on");
        let reversed = with(vec![off, on]);
        assert!(fold(&reversed, window("com.example.Editor")).border.is_some(), "and a later on beats an earlier off");
    }

    /// niri's `match is-active=true` recolours the window wearing the active paint: as focus moves,
    /// the colour goes with it, each window painted by its own focus.
    #[test]
    fn a_rule_for_the_active_window_recolours_it_as_focus_moves() {
        let settings = with(vec![DecorationRule {
            focus_ring: BandRule { active: Some(RED), inactive: Some(GREEN), ..BandRule::default() },
            ..rule(RuleMatch { is_active: Some(true), ..RuleMatch::default() })
        }]);
        let decoration = decoration(&settings, window("com.example.Editor"));
        let paint = |focused| crate::decorations::domain::style::Style::of(focused, true, &decoration).ring;
        assert_eq!(paint(true), Some(RED), "holding focus, the rule's active colour");
        assert_eq!(paint(false), Some(settings.focus_ring.band.inactive), "not holding it, the rule does not apply");
        let ring = fold(&settings, window("x")).ring.expect("on");
        assert_eq!(ring.active, settings.focus_ring.band.active, "a window as it opens holds no focus");
    }

    #[test]
    fn the_corner_radius_rule_overrides_the_measured_one() {
        let settings = with(vec![
            DecorationRule { corner_radius: Some(8.0), ..DecorationRule::default() },
            DecorationRule { corner_radius: Some(12.0), ..rule(app("com.example.Editor")) },
        ]);
        let editor = decoration(&settings, window("com.example.Editor"));
        assert_eq!(editor.corner(|| Some(26.0)), Some(12.0), "the later rule's, ahead of the window server's");
        assert_eq!(decoration(&settings, window("com.example.Other")).corner(|| Some(26.0)), Some(8.0));
        assert_eq!(decoration(&with(Vec::new()), window("x")).corner(|| Some(26.0)), Some(26.0), "no rule, the server's");
    }

    /// A floating window keeps its frame, so only a tiled window gives a border width out of a slot,
    /// whatever a rule for floating windows says.
    #[test]
    fn a_border_width_is_the_tiled_windows_whatever_a_floating_rule_says() {
        let settings = with(vec![
            DecorationRule { border: BandRule { on: Some(true), width: Some(2.0), ..BandRule::default() }, ..rule(app("com.example.Editor")) },
            DecorationRule {
                border: BandRule { width: Some(9.0), ..BandRule::default() },
                ..rule(RuleMatch { is_floating: Some(true), ..RuleMatch::default() })
            },
        ]);
        let floating = WindowRuleContext { state: RuleState { floating: true, focused: false }, ..window("com.example.Editor") };
        assert_eq!(border_width(&settings, floating), 2.0);
        assert_eq!(fold(&settings, floating).border.map(|band| band.width), Some(9.0), "drawn at the floating rule's");
        assert_eq!(border_width(&settings, window("com.example.Other")), 0.0, "borders off for the rest");
        assert!(shape_borders(&settings) && !shape_borders(&with(vec![DecorationRule { corner_radius: Some(4.0), ..DecorationRule::default() }])));
    }

    /// An open-time rule and a decoration rule written as one `window-rule` apply to the same windows.
    #[test]
    fn open_time_and_decoration_rules_match_through_one_matcher() {
        let matches = vec![app("com.example.Editor"), RuleMatch::title("^Prefs$")];
        let excludes = vec![RuleMatch { is_modal: Some(true), ..RuleMatch::default() }];
        let opening = WindowRule { matches: matches.clone(), excludes: excludes.clone(), open_floating: Some(true), ..WindowRule::default() };
        let decorating = DecorationRule { matches, excludes, corner_radius: Some(4.0), ..DecorationRule::default() };
        let engine = crate::windows::domain::rules::AppRuleEngine::new(&[opening]);
        let settings = with(vec![decorating]);
        let titled = WindowRuleContext { window_title: Some("Prefs"), ..window("com.example.Other") };
        let modal = WindowRuleContext { is_modal: true, ..window("com.example.Editor") };
        for context in [window("com.example.Editor"), titled, modal, window("com.example.Other")] {
            let opens = engine.evaluate(context) != crate::windows::domain::rules::AppRuleDecision::NoMatch;
            assert_eq!(opens, fold(&settings, context).radius.is_some(), "{context:?}");
        }
    }

    #[test]
    fn a_rule_that_only_colours_by_focus_follows_focus_and_decorates() {
        let focused = rule(RuleMatch { is_focused: Some(false), ..RuleMatch::default() });
        assert!(focused.follows_focus() && !focused.decorates());
        let excluded = DecorationRule { excludes: vec![RuleMatch { is_active: Some(true), ..RuleMatch::default() }], ..DecorationRule::default() };
        assert!(excluded.follows_focus(), "an exclude line turns with focus too");
        assert!(DecorationRule { corner_radius: Some(4.0), ..rule(app("x")) }.decorates());
        assert!(!rule(RuleMatch { is_floating: Some(true), ..RuleMatch::default() }).follows_focus());
    }
}
