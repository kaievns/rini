//! Keys, modifiers and hotkeys as rini names them.
//!
//! The vocabulary, and the parts of a key spec that need no keyboard: which modifiers a token names,
//! which side of a pair it means, how a `Hotkey` prints, how a spec written in `rini.toml` is
//! canonicalised. Turning a spec into a physical key is layout-dependent and lives in
//! `crate::input::platform::keyboard`.

use std::fmt;

use rustc_hash::FxHashMap as HashMap;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Copy, Clone, PartialEq, Eq, Hash)]
pub struct Modifiers(u8);

impl Modifiers {
    pub const ALT: Modifiers = Modifiers(0b0011_0000);
    pub const ALT_LEFT: Modifiers = Modifiers(0b0001_0000);
    pub const ALT_RIGHT: Modifiers = Modifiers(0b0010_0000);
    pub const CONTROL: Modifiers = Modifiers(0b0000_1100);
    pub const CONTROL_LEFT: Modifiers = Modifiers(0b0000_0100);
    pub const CONTROL_RIGHT: Modifiers = Modifiers(0b0000_1000);
    pub const META: Modifiers = Modifiers(0b1100_0000);
    pub const META_LEFT: Modifiers = Modifiers(0b0100_0000);
    pub const META_RIGHT: Modifiers = Modifiers(0b1000_0000);
    // Generic modifiers (match either left or right)
    pub const SHIFT: Modifiers = Modifiers(0b0000_0011);
    // Specific left/right modifier bits
    pub const SHIFT_LEFT: Modifiers = Modifiers(0b0000_0001);
    pub const SHIFT_RIGHT: Modifiers = Modifiers(0b0000_0010);

    pub fn empty() -> Self {
        Modifiers(0)
    }

    pub fn contains(&self, other: Modifiers) -> bool {
        (self.0 & other.0) == other.0
    }

    pub fn intersects(&self, other: Modifiers) -> bool {
        (self.0 & other.0) != 0
    }

    pub fn insert(&mut self, other: Modifiers) {
        self.0 |= other.0;
    }

    pub fn remove(&mut self, other: Modifiers) {
        self.0 &= !other.0;
    }

    pub fn has_generic_modifiers(&self) -> bool {
        MOD_FAMILIES.iter().any(|m| self.contains(m.generic))
    }

    pub fn expand_to_specific(&self) -> Vec<Modifiers> {
        let mut variants = vec![Modifiers::empty()];

        for m in MOD_FAMILIES {
            let has_generic = self.contains(m.generic);
            let has_left = self.contains(m.left);
            let has_right = self.contains(m.right);

            let left_allowed = has_left || has_generic;
            let right_allowed = has_right || has_generic;

            if left_allowed && right_allowed {
                let mut new_variants = Vec::with_capacity(variants.len() * 3);
                for v in &variants {
                    let mut vl = *v;
                    vl.insert(m.left);
                    new_variants.push(vl);

                    let mut vr = *v;
                    vr.insert(m.right);
                    new_variants.push(vr);

                    let mut vboth = *v;
                    vboth.insert(m.left);
                    vboth.insert(m.right);
                    new_variants.push(vboth);
                }
                variants = new_variants;
            } else if left_allowed {
                for v in &mut variants {
                    v.insert(m.left);
                }
            } else if right_allowed {
                for v in &mut variants {
                    v.insert(m.right);
                }
            }
        }

        variants
    }

    pub fn insert_from_token(&mut self, token: &str) -> bool {
        if let Some(mods) = modifier_from_token(token) {
            self.insert(mods);
            return true;
        }
        false
    }
}

impl fmt::Display for Modifiers {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut parts: Vec<&str> = Vec::new();

        for m in MOD_FAMILIES {
            let l = self.contains(m.left);
            let r = self.contains(m.right);

            match (l, r) {
                (true, true) => parts.push(m.name),
                (true, false) => parts.push(m.left_name),
                (false, true) => parts.push(m.right_name),
                (false, false) => {}
            }
        }

        write!(f, "{}", parts.join(" + "))
    }
}

#[derive(Clone, Copy)]
pub(in crate::input) struct ModFamily {
    pub(in crate::input) name: &'static str,
    pub(in crate::input) left_name: &'static str,
    pub(in crate::input) right_name: &'static str,

    pub(in crate::input) generic: Modifiers,
    pub(in crate::input) left: Modifiers,
    pub(in crate::input) right: Modifiers,

    pub(in crate::input) left_key: KeyCode,
    pub(in crate::input) right_key: KeyCode,

    /// The family's `CGEventFlags::Mask*`, as bits, so this table needs no CoreGraphics.
    /// `crate::input::platform::keyboard` has the test that proves each value right.
    pub(in crate::input) mask: u64,
    pub(in crate::input) left_mask: u64,
    pub(in crate::input) right_mask: u64,
}

pub(in crate::input) const MOD_FAMILIES: &[ModFamily] = &[
    ModFamily {
        name: "Ctrl",
        left_name: "CtrlLeft",
        right_name: "CtrlRight",
        generic: Modifiers::CONTROL,
        left: Modifiers::CONTROL_LEFT,
        right: Modifiers::CONTROL_RIGHT,
        left_key: KeyCode::ControlLeft,
        right_key: KeyCode::ControlRight,
        mask: 0x0004_0000, // CGEventFlags::MaskControl
        left_mask: 0x00000001,
        right_mask: 0x00002000,
    },
    ModFamily {
        name: "Alt",
        left_name: "AltLeft",
        right_name: "AltRight",
        generic: Modifiers::ALT,
        left: Modifiers::ALT_LEFT,
        right: Modifiers::ALT_RIGHT,
        left_key: KeyCode::AltLeft,
        right_key: KeyCode::AltRight,
        mask: 0x0008_0000, // CGEventFlags::MaskAlternate
        left_mask: 0x00000020,
        right_mask: 0x00000040,
    },
    ModFamily {
        name: "Shift",
        left_name: "ShiftLeft",
        right_name: "ShiftRight",
        generic: Modifiers::SHIFT,
        left: Modifiers::SHIFT_LEFT,
        right: Modifiers::SHIFT_RIGHT,
        left_key: KeyCode::ShiftLeft,
        right_key: KeyCode::ShiftRight,
        mask: 0x0002_0000, // CGEventFlags::MaskShift
        left_mask: 0x00000002,
        right_mask: 0x00000004,
    },
    ModFamily {
        name: "Meta",
        left_name: "MetaLeft",
        right_name: "MetaRight",
        generic: Modifiers::META,
        left: Modifiers::META_LEFT,
        right: Modifiers::META_RIGHT,
        left_key: KeyCode::MetaLeft,
        right_key: KeyCode::MetaRight,
        mask: 0x0010_0000, // CGEventFlags::MaskCommand
        left_mask: 0x00000008,
        right_mask: 0x00000010,
    },
];

pub(in crate::input) fn normalize_token(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

#[derive(Copy, Clone)]
pub(in crate::input) enum Side {
    Left,
    Right,
}

pub(in crate::input) fn split_side(token: &str) -> (Option<Side>, &str) {
    if let Some(rest) = token.strip_prefix("left") {
        return (Some(Side::Left), rest);
    }
    if let Some(rest) = token.strip_prefix("right") {
        return (Some(Side::Right), rest);
    }
    if let Some(rest) = token.strip_suffix("left") {
        return (Some(Side::Left), rest);
    }
    if let Some(rest) = token.strip_suffix("right") {
        return (Some(Side::Right), rest);
    }
    if let Some(rest) = token.strip_prefix('l') {
        return (Some(Side::Left), rest);
    }
    if let Some(rest) = token.strip_prefix('r') {
        return (Some(Side::Right), rest);
    }
    (None, token)
}

pub(in crate::input) fn modifier_from_token(token: &str) -> Option<Modifiers> {
    let t = normalize_token(token);
    let (side, base) = split_side(&t);
    let family = match base {
        "alt" | "option" => &MOD_FAMILIES[1],
        "ctrl" | "control" => &MOD_FAMILIES[0],
        "shift" => &MOD_FAMILIES[2],
        "meta" | "cmd" | "command" => &MOD_FAMILIES[3],
        _ => return None,
    };

    match side {
        None => Some(family.generic),
        Some(Side::Left) => Some(family.left),
        Some(Side::Right) => Some(family.right),
    }
}

#[derive(Serialize, Deserialize, Debug, Copy, Clone, PartialEq, Eq, Hash)]
pub enum KeyCode {
    KeyA,
    KeyS,
    KeyD,
    KeyF,
    KeyH,
    KeyG,
    KeyZ,
    KeyX,
    KeyC,
    KeyV,
    IntlBackslash,
    KeyB,
    KeyQ,
    KeyW,
    KeyE,
    KeyR,
    KeyY,
    KeyT,
    Digit1,
    Digit2,
    Digit3,
    Digit4,
    Digit6,
    Digit5,
    Equal,
    Digit9,
    Digit7,
    Minus,
    Digit8,
    Digit0,
    BracketRight,
    KeyO,
    KeyU,
    BracketLeft,
    KeyI,
    KeyP,
    Enter,
    KeyL,
    KeyJ,
    Quote,
    KeyK,
    Semicolon,
    Backslash,
    Comma,
    Slash,
    KeyN,
    KeyM,
    Period,
    Tab,
    Space,
    Backquote,
    Backspace,
    NumpadEnter,
    NumpadSubtract,
    Escape,
    MetaRight,
    MetaLeft,
    ShiftLeft,
    CapsLock,
    AltLeft,
    ControlLeft,
    ShiftRight,
    AltRight,
    ControlRight,
    Fn,
    F17,
    NumpadDecimal,
    NumpadMultiply,
    NumpadAdd,
    NumLock,
    AudioVolumeUp,
    AudioVolumeDown,
    AudioVolumeMute,
    NumpadDivide,
    F18,
    F19,
    NumpadEqual,
    Numpad0,
    Numpad1,
    Numpad2,
    Numpad3,
    Numpad4,
    Numpad5,
    Numpad6,
    Numpad7,
    F20,
    Numpad8,
    Numpad9,
    IntlYen,
    IntlRo,
    NumpadComma,
    F5,
    F6,
    F7,
    F3,
    F8,
    F9,
    Lang2,
    F11,
    Lang1,
    F13,
    F16,
    F14,
    F10,
    ContextMenu,
    F12,
    F15,
    Insert,
    Home,
    PageUp,
    Delete,
    F4,
    End,
    F2,
    PageDown,
    F1,
    ArrowLeft,
    ArrowRight,
    ArrowDown,
    ArrowUp,
}

impl fmt::Display for KeyCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        use KeyCode::*;
        let s = match self {
            KeyA => "A",
            KeyS => "S",
            KeyD => "D",
            KeyF => "F",
            KeyH => "H",
            KeyG => "G",
            KeyZ => "Z",
            KeyX => "X",
            KeyC => "C",
            KeyV => "V",
            KeyB => "B",
            KeyQ => "Q",
            KeyW => "W",
            KeyE => "E",
            KeyR => "R",
            KeyY => "Y",
            KeyT => "T",
            Digit1 => "1",
            Digit2 => "2",
            Digit3 => "3",
            Digit4 => "4",
            Digit5 => "5",
            Digit6 => "6",
            Digit7 => "7",
            Digit8 => "8",
            Digit9 => "9",
            Digit0 => "0",
            ArrowLeft => "Left",
            ArrowRight => "Right",
            ArrowUp => "Up",
            ArrowDown => "Down",
            Tab => "Tab",
            Space => "Space",
            Enter => "Enter",
            Escape => "Escape",
            _ => "Other",
        };
        write!(f, "{}", s)
    }
}

pub(in crate::input) const F_KEYS: [KeyCode; 20] = [
    KeyCode::F1,
    KeyCode::F2,
    KeyCode::F3,
    KeyCode::F4,
    KeyCode::F5,
    KeyCode::F6,
    KeyCode::F7,
    KeyCode::F8,
    KeyCode::F9,
    KeyCode::F10,
    KeyCode::F11,
    KeyCode::F12,
    KeyCode::F13,
    KeyCode::F14,
    KeyCode::F15,
    KeyCode::F16,
    KeyCode::F17,
    KeyCode::F18,
    KeyCode::F19,
    KeyCode::F20,
];

#[derive(Serialize, Debug, Clone, PartialEq, Eq, Hash)]
pub struct Hotkey {
    pub modifiers: Modifiers,
    pub key_code: KeyCode,
}

impl Hotkey {
    pub fn new(modifiers: Modifiers, key_code: KeyCode) -> Self {
        Self { modifiers, key_code }
    }
}

impl fmt::Display for Hotkey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.modifiers == Modifiers::empty() {
            write!(f, "{}", self.key_code)
        } else {
            write!(f, "{} + {}", self.modifiers, self.key_code)
        }
    }
}

#[derive(Serialize, Debug, Clone, PartialEq, Eq, Hash)]
pub enum HotkeySpec {
    Hotkey(Hotkey),
    ModifiersOnly { modifiers: Modifiers },
}

pub(in crate::input) fn default_key_for_modifiers(mods: Modifiers) -> Option<KeyCode> {
    for m in MOD_FAMILIES {
        if !mods.intersects(m.generic) {
            continue;
        }
        if mods.contains(m.right) && !mods.contains(m.left) {
            return Some(m.right_key);
        }
        return Some(m.left_key);
    }
    None
}

impl HotkeySpec {
    pub fn to_hotkey(&self) -> Option<Hotkey> {
        match self {
            HotkeySpec::Hotkey(h) => Some(h.clone()),
            HotkeySpec::ModifiersOnly { modifiers } => {
                default_key_for_modifiers(*modifiers).map(|k| Hotkey::new(*modifiers, k))
            }
        }
    }
}

impl From<HotkeySpec> for Hotkey {
    fn from(spec: HotkeySpec) -> Hotkey {
        match spec {
            HotkeySpec::Hotkey(h) => h,
            HotkeySpec::ModifiersOnly { modifiers } => {
                if let Some(k) = default_key_for_modifiers(modifiers) {
                    Hotkey::new(modifiers, k)
                } else {
                    Hotkey::new(modifiers, KeyCode::ShiftLeft)
                }
            }
        }
    }
}

/// Whether a key is a modifier: one side of a family, or one of the three lock keys macOS reports
/// as modifier flags rather than as key presses.
pub fn is_modifier_key(key_code: KeyCode) -> bool {
    MOD_FAMILIES.iter().any(|m| key_code == m.left_key || key_code == m.right_key)
        || matches!(key_code, KeyCode::CapsLock | KeyCode::Fn | KeyCode::NumLock)
}

/// One token of a key spec, as it should be WRITTEN.
///
/// Not to be confused with `normalize_token`, which folds a token for MATCHING a modifier name and
/// therefore lower-cases. This one produces the canonical spelling: a single letter upper-cases,
/// because `a` and `A` name the same physical key and the config may be written either way, and an
/// arrow word becomes its `ArrowX` name.
///
/// Anything else is left exactly as written. A token this does not know is either a key name the
/// keyboard layer will resolve or a typo the config validator will refuse, and guessing here would
/// turn the second into the first.
fn canonical_spec_token(word: &str) -> String {
    if word.len() == 1 {
        return word.to_ascii_uppercase();
    }
    match word.to_lowercase().as_str() {
        "up" => "ArrowUp".to_owned(),
        "down" => "ArrowDown".to_owned(),
        "left" => "ArrowLeft".to_owned(),
        "right" => "ArrowRight".to_owned(),
        _ => word.to_owned(),
    }
}

/// Canonicalises a key spec as written in `rini.toml`: single letters upper-cased, arrow words
/// to `ArrowUp` and kin.
///
/// Splits on anything non-alphabetic and puts the separators back unchanged, so `Alt + Shift + Down`
/// keeps its spacing and its pluses. The token rule was written out twice here — once for the words
/// inside the loop and once for a trailing word with no separator after it — which is two places for
/// one rule to be fixed in.
pub fn normalize_spec(key: &str) -> String {
    let mut out = String::with_capacity(key.len());
    let mut word = String::new();

    for ch in key.chars() {
        if ch.is_alphabetic() {
            word.push(ch);
            continue;
        }
        if !word.is_empty() {
            out.push_str(&canonical_spec_token(&word));
            word.clear();
        }
        out.push(ch);
    }
    if !word.is_empty() {
        out.push_str(&canonical_spec_token(&word));
    }

    out
}

/// Replaces a leading `[modifier_combinations]` alias (`comb1 + C`) with its definition.
pub fn expand_modifier_combination(key: &str, combinations: &HashMap<String, String>) -> String {
    if let Some(plus_pos) = key.find(" + ") {
        let potential_combo = &key[..plus_pos];
        if let Some(combo_value) = combinations.get(potential_combo) {
            let rest = &key[plus_pos + 3..];
            return format!("{} + {}", combo_value, rest);
        }
    }
    key.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arrow_words_and_single_letters_are_canonicalised() {
        assert_eq!(normalize_spec("Alt + Shift + Down"), "Alt + Shift + ArrowDown");
        assert_eq!(normalize_spec("Ctrl + Up"), "Ctrl + ArrowUp");
        assert_eq!(normalize_spec("Shift + Left"), "Shift + ArrowLeft");
        assert_eq!(normalize_spec("Meta + Right"), "Meta + ArrowRight");
    }

    // --- the bitfield -------------------------------------------------------------------------

    /// A generic modifier is BOTH sides, which is what makes `contains(CONTROL)` true for a press of
    /// either one. Every family has to agree on that or a binding written `Ctrl` matches one side.
    #[test]
    fn every_generic_modifier_is_exactly_its_two_sides() {
        for family in MOD_FAMILIES {
            let mut both = Modifiers::empty();
            both.insert(family.left);
            both.insert(family.right);
            assert_eq!(both, family.generic, "{} is not its own two sides", family.name);
        }
    }

    /// No two families share a bit. A collision would make Ctrl and Alt the same modifier.
    #[test]
    fn no_two_families_share_a_bit() {
        let mut seen = Modifiers::empty();
        for family in MOD_FAMILIES {
            assert!(
                !seen.intersects(family.generic),
                "{} overlaps another family",
                family.name
            );
            seen.insert(family.generic);
        }
    }

    #[test]
    fn one_side_does_not_contain_the_generic_pair() {
        let mut left_only = Modifiers::empty();
        left_only.insert(Modifiers::CONTROL_LEFT);
        assert!(!left_only.contains(Modifiers::CONTROL), "one side is not both");
        assert!(left_only.intersects(Modifiers::CONTROL), "but it is one of them");
    }

    #[test]
    fn removing_a_modifier_leaves_the_others() {
        let mut mods = Modifiers::empty();
        mods.insert(Modifiers::CONTROL);
        mods.insert(Modifiers::SHIFT);
        mods.remove(Modifiers::CONTROL);
        assert!(!mods.intersects(Modifiers::CONTROL));
        assert!(mods.contains(Modifiers::SHIFT));
    }

    #[test]
    fn a_specific_side_is_not_generic() {
        let mut left = Modifiers::empty();
        left.insert(Modifiers::ALT_LEFT);
        assert!(!left.has_generic_modifiers());
        let mut generic = Modifiers::empty();
        generic.insert(Modifiers::ALT);
        assert!(generic.has_generic_modifiers());
    }

    // --- expanding a generic binding ----------------------------------------------------------

    /// `Ctrl + A` has to match left Ctrl, right Ctrl, or both held at once — three variants, because
    /// macOS reports the side and the binding did not name one.
    #[test]
    fn a_generic_modifier_expands_to_three_variants() {
        let mut mods = Modifiers::empty();
        mods.insert(Modifiers::CONTROL);
        let variants = mods.expand_to_specific();
        assert_eq!(variants.len(), 3);
        assert!(variants.iter().any(|v| *v == Modifiers::CONTROL_LEFT));
        assert!(variants.iter().any(|v| *v == Modifiers::CONTROL_RIGHT));
        assert!(variants.iter().any(|v| v.contains(Modifiers::CONTROL)));
    }

    /// A binding that names a side has exactly one reading. Expanding it to both would make
    /// `CtrlLeft + A` fire on right Ctrl, which is the whole point of writing the side.
    #[test]
    fn a_named_side_expands_to_itself_only() {
        let mut mods = Modifiers::empty();
        mods.insert(Modifiers::CONTROL_LEFT);
        assert_eq!(mods.expand_to_specific(), vec![Modifiers::CONTROL_LEFT]);
    }

    /// Two generic modifiers multiply: 3 x 3. This is the count that makes a three-modifier generic
    /// binding 27 registrations, which is why the specific form is worth writing.
    #[test]
    fn two_generic_modifiers_multiply_their_variants() {
        let mut mods = Modifiers::empty();
        mods.insert(Modifiers::CONTROL);
        mods.insert(Modifiers::SHIFT);
        assert_eq!(mods.expand_to_specific().len(), 9);
    }

    #[test]
    fn no_modifiers_expands_to_one_empty_variant() {
        assert_eq!(Modifiers::empty().expand_to_specific(), vec![Modifiers::empty()]);
    }

    #[test]
    fn every_expansion_of_a_generic_modifier_presses_that_family() {
        let mut mods = Modifiers::empty();
        mods.insert(Modifiers::META);
        for variant in mods.expand_to_specific() {
            assert!(
                variant.intersects(Modifiers::META),
                "{variant} does not press Meta"
            );
        }
    }

    // --- tokens -------------------------------------------------------------------------------

    #[test]
    fn every_modifier_name_and_alias_parses() {
        for token in [
            "ctrl", "control", "alt", "option", "shift", "meta", "cmd", "command",
        ] {
            assert!(modifier_from_token(token).is_some(), "{token} does not parse");
        }
    }

    /// The aliases are the same modifier, not merely both valid.
    #[test]
    fn an_alias_is_the_same_modifier_as_the_name_it_aliases() {
        assert_eq!(modifier_from_token("option"), modifier_from_token("alt"));
        assert_eq!(modifier_from_token("cmd"), modifier_from_token("meta"));
        assert_eq!(modifier_from_token("command"), modifier_from_token("meta"));
        assert_eq!(modifier_from_token("control"), modifier_from_token("ctrl"));
    }

    #[test]
    fn a_side_prefix_names_that_side() {
        assert_eq!(modifier_from_token("lctrl"), Some(Modifiers::CONTROL_LEFT));
        assert_eq!(modifier_from_token("rctrl"), Some(Modifiers::CONTROL_RIGHT));
        assert_eq!(modifier_from_token("ctrl"), Some(Modifiers::CONTROL));
    }

    #[test]
    fn a_token_that_is_not_a_modifier_does_not_parse() {
        for token in ["a", "ArrowUp", "hyper", "", "ctr"] {
            assert_eq!(
                modifier_from_token(token),
                None,
                "{token:?} should not be a modifier"
            );
        }
    }

    #[test]
    fn inserting_from_a_token_reports_whether_it_was_one() {
        let mut mods = Modifiers::empty();
        assert!(mods.insert_from_token("ctrl"));
        assert!(mods.contains(Modifiers::CONTROL));
        assert!(!mods.insert_from_token("A"));
        assert!(
            mods.contains(Modifiers::CONTROL),
            "a non-modifier leaves the set alone"
        );
    }

    // --- printing -----------------------------------------------------------------------------

    /// A `Hotkey` prints as something the config could have been written as, which is what makes the
    /// logs and `rini debug` readable. Both sides print as the generic name.
    #[test]
    fn both_sides_print_as_the_generic_name() {
        let mut mods = Modifiers::empty();
        mods.insert(Modifiers::CONTROL);
        assert_eq!(mods.to_string(), "Ctrl");
    }

    #[test]
    fn one_side_prints_as_that_side() {
        let mut mods = Modifiers::empty();
        mods.insert(Modifiers::CONTROL_LEFT);
        assert_eq!(mods.to_string(), "CtrlLeft");
        let mut right = Modifiers::empty();
        right.insert(Modifiers::SHIFT_RIGHT);
        assert_eq!(right.to_string(), "ShiftRight");
    }

    /// The print order is the family order, not the insertion order, so the same set always prints
    /// the same way and a log line can be compared against a config line.
    #[test]
    fn modifiers_print_in_a_fixed_order_whatever_order_they_went_in() {
        let mut one = Modifiers::empty();
        one.insert(Modifiers::META);
        one.insert(Modifiers::CONTROL);
        let mut other = Modifiers::empty();
        other.insert(Modifiers::CONTROL);
        other.insert(Modifiers::META);
        assert_eq!(one.to_string(), other.to_string());
        assert_eq!(one.to_string(), "Ctrl + Meta");
    }

    #[test]
    fn no_modifiers_prints_as_nothing() {
        assert_eq!(Modifiers::empty().to_string(), "");
    }

    // --- a modifiers-only binding needs a key -------------------------------------------------

    /// A binding with no key registers against one anyway: macOS has no "modifier alone" hotkey, so
    /// rini picks the modifier's own key code.
    #[test]
    fn a_modifiers_only_spec_becomes_a_hotkey_on_the_modifiers_own_key() {
        let mut mods = Modifiers::empty();
        mods.insert(Modifiers::CONTROL);
        let hotkey = HotkeySpec::ModifiersOnly { modifiers: mods }.to_hotkey().expect("a hotkey");
        assert_eq!(hotkey.key_code, KeyCode::ControlLeft);
    }

    /// Naming only the right side picks the right key, not the left one.
    #[test]
    fn a_right_side_only_spec_picks_the_right_key() {
        let mut mods = Modifiers::empty();
        mods.insert(Modifiers::CONTROL_RIGHT);
        assert_eq!(default_key_for_modifiers(mods), Some(KeyCode::ControlRight));
    }

    #[test]
    fn a_spec_with_no_modifiers_at_all_has_no_default_key() {
        assert_eq!(default_key_for_modifiers(Modifiers::empty()), None);
    }

    // --- is_modifier_key ----------------------------------------------------------------------

    /// Both sides of every family, plus the three lock keys macOS reports as flags rather than as
    /// presses. Missing one means a modifier press is treated as a key press and fires a binding.
    #[test]
    fn every_side_of_every_family_is_a_modifier_key() {
        for family in MOD_FAMILIES {
            assert!(is_modifier_key(family.left_key), "{} left", family.name);
            assert!(is_modifier_key(family.right_key), "{} right", family.name);
        }
        for lock in [KeyCode::CapsLock, KeyCode::Fn, KeyCode::NumLock] {
            assert!(is_modifier_key(lock), "{lock:?}");
        }
    }

    #[test]
    fn an_ordinary_key_is_not_a_modifier() {
        for key in [
            KeyCode::KeyA,
            KeyCode::Digit1,
            KeyCode::ArrowUp,
            KeyCode::Space,
        ] {
            assert!(!is_modifier_key(key), "{key:?}");
        }
    }

    // --- spec canonicalisation ----------------------------------------------------------------

    #[test]
    fn a_single_letter_is_upper_cased_whatever_it_arrives_as() {
        assert_eq!(normalize_spec("ctrl + a"), "ctrl + A");
        assert_eq!(normalize_spec("Ctrl + A"), "Ctrl + A");
    }

    /// A trailing word takes the same rule as a word inside the spec. This was written out twice, and
    /// a fix to one copy and not the other is the failure the duplication invited.
    #[test]
    fn the_last_token_is_canonicalised_the_same_as_the_others() {
        assert_eq!(normalize_spec("Alt + down"), "Alt + ArrowDown");
        assert_eq!(normalize_spec("down + Alt"), "ArrowDown + Alt");
        assert_eq!(normalize_spec("down"), "ArrowDown");
        assert_eq!(normalize_spec("a"), "A");
    }

    /// Separators come back exactly as written, so canonicalising does not reformat the user's spec.
    #[test]
    fn spacing_and_separators_survive_untouched() {
        assert_eq!(normalize_spec("Ctrl+a"), "Ctrl+A");
        assert_eq!(normalize_spec("Ctrl  +  a"), "Ctrl  +  A");
    }

    /// A word this does not know is left alone, because it is either a key name the keyboard layer
    /// resolves or a typo the config validator refuses. Guessing would turn the second into the first.
    #[test]
    fn an_unknown_word_is_left_exactly_as_written() {
        assert_eq!(normalize_spec("Ctrl + Hyper"), "Ctrl + Hyper");
        assert_eq!(normalize_spec("Ctrl + F13"), "Ctrl + F13");
    }

    #[test]
    fn an_empty_spec_canonicalises_to_nothing() {
        assert_eq!(normalize_spec(""), "");
    }

    #[test]
    fn canonicalising_twice_changes_nothing_the_second_time() {
        for spec in ["Alt + Shift + Down", "ctrl+a", "down", "Ctrl + F13", ""] {
            let once = normalize_spec(spec);
            assert_eq!(normalize_spec(&once), once, "{spec:?} is not stable");
        }
    }

    // --- modifier combinations ----------------------------------------------------------------

    fn combos() -> HashMap<String, String> {
        let mut map = HashMap::default();
        map.insert("hyper".to_owned(), "Ctrl + Alt + Shift + Meta".to_owned());
        map
    }

    #[test]
    fn a_leading_alias_is_replaced_by_its_definition() {
        assert_eq!(
            expand_modifier_combination("hyper + C", &combos()),
            "Ctrl + Alt + Shift + Meta + C"
        );
    }

    #[test]
    fn a_spec_naming_no_alias_is_returned_unchanged() {
        assert_eq!(expand_modifier_combination("Ctrl + C", &combos()), "Ctrl + C");
    }

    /// Only a LEADING alias is expanded. An alias later in the spec is left alone, which is a real
    /// limit rather than an oversight: a combination names the modifiers a binding starts with, and
    /// substituting one mid-spec would put modifiers after the key.
    #[test]
    fn an_alias_that_is_not_leading_is_left_alone() {
        assert_eq!(
            expand_modifier_combination("Ctrl + hyper", &combos()),
            "Ctrl + hyper"
        );
    }

    /// A bare alias with no key after it is also left alone, because the split looks for ` + `. Worth
    /// knowing: `"hyper" = "..."` used as a whole binding does not expand.
    #[test]
    fn a_bare_alias_with_no_key_is_not_expanded() {
        assert_eq!(expand_modifier_combination("hyper", &combos()), "hyper");
    }
}
