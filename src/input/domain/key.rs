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

/// Canonicalises a key spec as written in `rini.toml`: single letters upper-cased, arrow words
/// to `ArrowUp` and kin.
pub fn normalize_spec(key: &str) -> String {
    let mut out = String::with_capacity(key.len());
    let mut word = String::new();

    for ch in key.chars() {
        if ch.is_alphabetic() {
            word.push(ch);
        } else {
            if !word.is_empty() {
                let token = if word.len() == 1 {
                    word.to_ascii_uppercase()
                } else {
                    match word.to_lowercase().as_str() {
                        "up" => "ArrowUp".to_string(),
                        "down" => "ArrowDown".to_string(),
                        "left" => "ArrowLeft".to_string(),
                        "right" => "ArrowRight".to_string(),
                        _ => word.clone(),
                    }
                };
                out.push_str(&token);
                word.clear();
            }
            out.push(ch);
        }
    }

    if !word.is_empty() {
        let token = if word.len() == 1 {
            word.to_ascii_uppercase()
        } else {
            match word.to_lowercase().as_str() {
                "up" => "ArrowUp".to_string(),
                "down" => "ArrowDown".to_string(),
                "left" => "ArrowLeft".to_string(),
                "right" => "ArrowRight".to_string(),
                _ => word.clone(),
            }
        };
        out.push_str(&token);
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
        assert_eq!(
            normalize_spec("Alt + Shift + Down"),
            "Alt + Shift + ArrowDown"
        );
        assert_eq!(normalize_spec("Ctrl + Up"), "Ctrl + ArrowUp");
        assert_eq!(
            normalize_spec("Shift + Left"),
            "Shift + ArrowLeft"
        );
        assert_eq!(
            normalize_spec("Meta + Right"),
            "Meta + ArrowRight"
        );
    }
}
