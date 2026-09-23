//! Turning a keystroke, or a key spec, into a physical key.
//!
//! Both need the live keyboard. `"a"` has to mean the key that produces `a` on the layout in use, and
//! a CGEvent reports a virtual keycode only the layout can name, so the `FromStr` impls live here
//! rather than beside the types they build in `crate::input::domain::key`.

use std::collections::HashMap as StdHashMap;
use std::ffi::c_void;
use std::ptr::NonNull;
use std::str::FromStr;
use std::sync::LazyLock;

use anyhow::anyhow;
use objc2_core_foundation::CFData;
use objc2_core_graphics::{CGEvent, CGEventField, CGEventFlags};
use parking_lot::Mutex;
use serde::Deserialize;

use crate::input::domain::key::{
    F_KEYS, Hotkey, HotkeySpec, KeyCode, MOD_FAMILIES, Modifiers, normalize_token,
};

impl FromStr for KeyCode {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.trim();
        if s.is_empty() {
            return Err(anyhow!("Unrecognized key token: <empty>"));
        }

        if s.chars().count() == 1 {
            if let Some(k) = keycode_from_char(s) {
                return Ok(k);
            }

            return Err(anyhow!("carbon keymap failed"));
        }

        let t = normalize_token(s);

        // function keys
        if let Some(rest) = t.strip_prefix('f') {
            if let Ok(n) = rest.parse::<u8>() {
                if (1..=20).contains(&n) {
                    return Ok(F_KEYS[(n - 1) as usize]);
                }
            }
        }

        let key = match t.as_str() {
            "left" | "arrowleft" => KeyCode::ArrowLeft,
            "right" | "arrowright" => KeyCode::ArrowRight,
            "up" | "arrowup" => KeyCode::ArrowUp,
            "down" | "arrowdown" => KeyCode::ArrowDown,

            "tab" => KeyCode::Tab,
            "space" => KeyCode::Space,
            "enter" | "return" => KeyCode::Enter,
            "esc" | "escape" => KeyCode::Escape,
            "fn" => KeyCode::Fn,

            "pageup" => KeyCode::PageUp,
            "pagedown" => KeyCode::PageDown,
            "home" => KeyCode::Home,
            "end" => KeyCode::End,
            "insert" => KeyCode::Insert,
            "delete" | "del" => KeyCode::Delete,

            "minus" | "hyphen" => layout_char_keycode("-", KeyCode::Minus),
            "equal" | "equals" => layout_char_keycode("=", KeyCode::Equal),
            "comma" => layout_char_keycode(",", KeyCode::Comma),
            "period" | "dot" => layout_char_keycode(".", KeyCode::Period),
            "slash" | "forwardslash" => layout_char_keycode("/", KeyCode::Slash),
            "semicolon" => layout_char_keycode(";", KeyCode::Semicolon),
            "quote" | "apostrophe" => layout_char_keycode("'", KeyCode::Quote),
            "backquote" | "grave" | "tilde" => layout_char_keycode("`", KeyCode::Backquote),
            "backslash" => layout_char_keycode("\\", KeyCode::Backslash),
            "bracketleft" | "leftbracket" | "leftsquarebracket" => {
                layout_char_keycode("[", KeyCode::BracketLeft)
            }
            "bracketright" | "rightbracket" | "rightsquarebracket" => {
                layout_char_keycode("]", KeyCode::BracketRight)
            }

            other => return Err(anyhow!("Unrecognized key token: {}", other)),
        };

        Ok(key)
    }
}

fn layout_char_keycode(ch: &str, fallback: KeyCode) -> KeyCode {
    keycode_from_char(ch).unwrap_or(fallback)
}

fn parse_mods_and_optional_key(s: &str) -> Result<(Modifiers, Option<KeyCode>), anyhow::Error> {
    let parts: Vec<&str> = s.split('+').map(|p| p.trim()).filter(|p| !p.is_empty()).collect();

    let mut mods = Modifiers::empty();
    let mut key_opt: Option<KeyCode> = None;

    for part in parts {
        if mods.insert_from_token(part) {
            continue;
        }
        let code = KeyCode::from_str(part)?;
        key_opt = Some(code);
    }

    Ok((mods, key_opt))
}

impl FromStr for Hotkey {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (mods, key_opt) = parse_mods_and_optional_key(s)?;
        let key_code = key_opt.ok_or_else(|| anyhow!("No key specified in hotkey: {}", s))?;
        Ok(Hotkey::new(mods, key_code))
    }
}

impl<'de> Deserialize<'de> for Hotkey {
    fn deserialize<D>(deserializer: D) -> Result<Hotkey, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum HotkeyRepr {
            Str(String),
            Map {
                modifiers: Modifiers,
                key_code: KeyCode,
            },
        }

        let repr = HotkeyRepr::deserialize(deserializer)?;
        match repr {
            HotkeyRepr::Str(s) => Hotkey::from_str(&s).map_err(serde::de::Error::custom),
            HotkeyRepr::Map { modifiers, key_code } => Ok(Hotkey::new(modifiers, key_code)),
        }
    }
}

pub fn modifiers_from_flags(flags: CGEventFlags) -> Modifiers {
    let mut mods = Modifiers::empty();
    for m in MOD_FAMILIES {
        if m.is_active(flags.bits()) {
            mods.insert(m.generic);
        }
    }
    mods
}

pub fn modifiers_from_flags_with_keys<S: std::hash::BuildHasher>(
    flags: CGEventFlags,
    pressed_keys: &std::collections::HashSet<KeyCode, S>,
) -> Modifiers {
    let mut mods = Modifiers::empty();

    for m in MOD_FAMILIES {
        if !m.is_active(flags.bits()) {
            continue;
        }

        // Prefer the tracked key set during normal event processing, while
        // retaining side information directly from the event flags when
        // recovering after a dropped event or re-enabled tap.
        let has_left = pressed_keys.contains(&m.left_key) || m.left_is_active(flags.bits());
        let has_right = pressed_keys.contains(&m.right_key) || m.right_is_active(flags.bits());

        if has_left {
            mods.insert(m.left);
        }
        if has_right {
            mods.insert(m.right);
        }

        if !has_left && !has_right {
            mods.insert(m.left);
        }
    }

    mods
}

pub fn modifier_flag_for_key(key_code: KeyCode) -> Option<CGEventFlags> {
    crate::input::domain::key::modifier_mask_for_key(key_code).map(CGEventFlags)
}

/// Returns whether the specific physical modifier key is active.
///
/// The device-independent Core Graphics masks only identify a modifier
/// family. The low device-dependent bits retain the left/right distinction.
pub fn modifier_key_is_active(flags: CGEventFlags, key_code: KeyCode) -> bool {
    crate::input::domain::key::modifier_key_is_active(flags.bits(), key_code)
}

pub fn key_code_from_event(event: &CGEvent) -> Option<KeyCode> {
    let raw = CGEvent::integer_value_field(Some(event), CGEventField::KeyboardEventKeycode);
    if raw < 0 {
        return None;
    }
    cg_keycode_to_keycode(raw as u16)
}

pub fn cg_keycode_to_keycode(code: u16) -> Option<KeyCode> {
    CG_KEYCODE_TABLE.get(code as usize).copied().flatten()
}

const fn build_cg_keycode_table() -> [Option<KeyCode>; 0x80] {
    let mut t: [Option<KeyCode>; 0x80] = [None; 0x80];

    t[0x00] = Some(KeyCode::KeyA);
    t[0x01] = Some(KeyCode::KeyS);
    t[0x02] = Some(KeyCode::KeyD);
    t[0x03] = Some(KeyCode::KeyF);
    t[0x04] = Some(KeyCode::KeyH);
    t[0x05] = Some(KeyCode::KeyG);
    t[0x06] = Some(KeyCode::KeyZ);
    t[0x07] = Some(KeyCode::KeyX);
    t[0x08] = Some(KeyCode::KeyC);
    t[0x09] = Some(KeyCode::KeyV);
    t[0x0A] = Some(KeyCode::IntlBackslash);
    t[0x0B] = Some(KeyCode::KeyB);
    t[0x0C] = Some(KeyCode::KeyQ);
    t[0x0D] = Some(KeyCode::KeyW);
    t[0x0E] = Some(KeyCode::KeyE);
    t[0x0F] = Some(KeyCode::KeyR);
    t[0x10] = Some(KeyCode::KeyY);
    t[0x11] = Some(KeyCode::KeyT);
    t[0x12] = Some(KeyCode::Digit1);
    t[0x13] = Some(KeyCode::Digit2);
    t[0x14] = Some(KeyCode::Digit3);
    t[0x15] = Some(KeyCode::Digit4);
    t[0x16] = Some(KeyCode::Digit6);
    t[0x17] = Some(KeyCode::Digit5);
    t[0x18] = Some(KeyCode::Equal);
    t[0x19] = Some(KeyCode::Digit9);
    t[0x1A] = Some(KeyCode::Digit7);
    t[0x1B] = Some(KeyCode::Minus);
    t[0x1C] = Some(KeyCode::Digit8);
    t[0x1D] = Some(KeyCode::Digit0);
    t[0x1E] = Some(KeyCode::BracketRight);
    t[0x1F] = Some(KeyCode::KeyO);
    t[0x20] = Some(KeyCode::KeyU);
    t[0x21] = Some(KeyCode::BracketLeft);
    t[0x22] = Some(KeyCode::KeyI);
    t[0x23] = Some(KeyCode::KeyP);
    t[0x24] = Some(KeyCode::Enter);
    t[0x25] = Some(KeyCode::KeyL);
    t[0x26] = Some(KeyCode::KeyJ);
    t[0x27] = Some(KeyCode::Quote);
    t[0x28] = Some(KeyCode::KeyK);
    t[0x29] = Some(KeyCode::Semicolon);
    t[0x2A] = Some(KeyCode::Backslash);
    t[0x2B] = Some(KeyCode::Comma);
    t[0x2C] = Some(KeyCode::Slash);
    t[0x2D] = Some(KeyCode::KeyN);
    t[0x2E] = Some(KeyCode::KeyM);
    t[0x2F] = Some(KeyCode::Period);
    t[0x30] = Some(KeyCode::Tab);
    t[0x31] = Some(KeyCode::Space);
    t[0x32] = Some(KeyCode::Backquote);
    t[0x33] = Some(KeyCode::Backspace);
    t[0x34] = Some(KeyCode::NumpadEnter);
    t[0x35] = Some(KeyCode::Escape);
    t[0x36] = Some(KeyCode::MetaRight);
    t[0x37] = Some(KeyCode::MetaLeft);
    t[0x38] = Some(KeyCode::ShiftLeft);
    t[0x39] = Some(KeyCode::CapsLock);
    t[0x3A] = Some(KeyCode::AltLeft);
    t[0x3B] = Some(KeyCode::ControlLeft);
    t[0x3C] = Some(KeyCode::ShiftRight);
    t[0x3D] = Some(KeyCode::AltRight);
    t[0x3E] = Some(KeyCode::ControlRight);
    t[0x3F] = Some(KeyCode::Fn);
    t[0x40] = Some(KeyCode::F17);
    t[0x41] = Some(KeyCode::NumpadDecimal);
    t[0x43] = Some(KeyCode::NumpadMultiply);
    t[0x45] = Some(KeyCode::NumpadAdd);
    t[0x47] = Some(KeyCode::NumLock);
    t[0x48] = Some(KeyCode::AudioVolumeUp);
    t[0x49] = Some(KeyCode::AudioVolumeDown);
    t[0x4A] = Some(KeyCode::AudioVolumeMute);
    t[0x4B] = Some(KeyCode::NumpadDivide);
    t[0x4C] = Some(KeyCode::NumpadEnter);
    t[0x4E] = Some(KeyCode::NumpadSubtract);
    t[0x4F] = Some(KeyCode::F18);
    t[0x50] = Some(KeyCode::F19);
    t[0x51] = Some(KeyCode::NumpadEqual);
    t[0x52] = Some(KeyCode::Numpad0);
    t[0x53] = Some(KeyCode::Numpad1);
    t[0x54] = Some(KeyCode::Numpad2);
    t[0x55] = Some(KeyCode::Numpad3);
    t[0x56] = Some(KeyCode::Numpad4);
    t[0x57] = Some(KeyCode::Numpad5);
    t[0x58] = Some(KeyCode::Numpad6);
    t[0x59] = Some(KeyCode::Numpad7);
    t[0x5A] = Some(KeyCode::F20);
    t[0x5B] = Some(KeyCode::Numpad8);
    t[0x5C] = Some(KeyCode::Numpad9);
    t[0x5D] = Some(KeyCode::IntlYen);
    t[0x5E] = Some(KeyCode::IntlRo);
    t[0x5F] = Some(KeyCode::NumpadComma);
    t[0x60] = Some(KeyCode::F5);
    t[0x61] = Some(KeyCode::F6);
    t[0x62] = Some(KeyCode::F7);
    t[0x63] = Some(KeyCode::F3);
    t[0x64] = Some(KeyCode::F8);
    t[0x65] = Some(KeyCode::F9);
    t[0x66] = Some(KeyCode::Lang2);
    t[0x67] = Some(KeyCode::F11);
    t[0x68] = Some(KeyCode::Lang1);
    t[0x69] = Some(KeyCode::F13);
    t[0x6A] = Some(KeyCode::F16);
    t[0x6B] = Some(KeyCode::F14);
    t[0x6D] = Some(KeyCode::F10);
    t[0x6E] = Some(KeyCode::ContextMenu);
    t[0x6F] = Some(KeyCode::F12);
    t[0x71] = Some(KeyCode::F15);
    t[0x72] = Some(KeyCode::Insert);
    t[0x73] = Some(KeyCode::Home);
    t[0x74] = Some(KeyCode::PageUp);
    t[0x75] = Some(KeyCode::Delete);
    t[0x76] = Some(KeyCode::F4);
    t[0x77] = Some(KeyCode::End);
    t[0x78] = Some(KeyCode::F2);
    t[0x79] = Some(KeyCode::PageDown);
    t[0x7A] = Some(KeyCode::F1);
    t[0x7B] = Some(KeyCode::ArrowLeft);
    t[0x7C] = Some(KeyCode::ArrowRight);
    t[0x7D] = Some(KeyCode::ArrowDown);
    t[0x7E] = Some(KeyCode::ArrowUp);

    t
}

const CG_KEYCODE_TABLE: [Option<KeyCode>; 0x80] = build_cg_keycode_table();

#[cfg(target_os = "macos")]
type CFStringRef = *const c_void;

#[cfg(target_os = "macos")]
#[link(name = "Carbon", kind = "framework")]
unsafe extern "C" {
    fn TISCopyCurrentASCIICapableKeyboardLayoutInputSource() -> *mut c_void;
    fn TISGetInputSourceProperty(keyboard: *const c_void, property: CFStringRef) -> *mut c_void;
    fn UCKeyTranslate(
        keyLayoutPtr: *const u8,
        virtualKeyCode: u16,
        keyAction: u16,
        modifierKeyState: u32,
        keyboardType: u32,
        keyTranslateOptions: u32,
        deadKeyState: *mut u32,
        maxStringLength: usize,
        actualStringLength: *mut isize,
        unicodeString: *mut u16,
    ) -> i32;
    fn LMGetKbdType() -> u8;
    static kTISPropertyUnicodeKeyLayoutData: CFStringRef;
}

#[cfg(target_os = "macos")]
const VIRTUAL_KEYCODE_NUMS: &[u16] = &[
    0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0A, 0x0B, 0x0C, 0x0D, 0x0E, 0x0F,
    0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1A, 0x1B, 0x1C, 0x1D, 0x1E, 0x1F,
    0x20, 0x21, 0x22, 0x23, 0x25, 0x26, 0x27, 0x28, 0x29, 0x2A, 0x2B, 0x2C, 0x2D, 0x2E, 0x2F,
    0x32, // backquote
    // keypad subset
    0x41, 0x43, 0x45, 0x47, 0x4B, 0x4C, 0x4E, 0x51, 0x52, 0x53, 0x54, 0x55, 0x56, 0x57, 0x58, 0x59,
    0x5B, 0x5C,
];

#[cfg(target_os = "macos")]
fn generate_virtual_keymap() -> StdHashMap<String, KeyCode> {
    static KEYMAP_GENERATION_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));
    let _guard = KEYMAP_GENERATION_LOCK.lock();
    let mut keymap = StdHashMap::new();

    let keyboard = unsafe { TISCopyCurrentASCIICapableKeyboardLayoutInputSource() };
    if keyboard.is_null() {
        tracing::warn!("Could not get ASCII-capable keyboard layout input source");
        return keymap;
    }

    let layout_data = NonNull::new(unsafe {
        TISGetInputSourceProperty(keyboard, kTISPropertyUnicodeKeyLayoutData).cast::<CFData>()
    });

    let Some(layout_data) = layout_data else {
        tracing::warn!("Could not get keyboard layout data");
        unsafe {
            rini_skylight_sys::CFRelease(keyboard.cast());
        }
        return keymap;
    };

    let layout_ptr = unsafe { CFData::byte_ptr(layout_data.as_ref()) };

    const K_UC_KEY_ACTION_DOWN: u16 = 0;
    const K_UC_NO_DEAD_KEYS: u32 = 1;

    let kbd_type: u32 = unsafe { LMGetKbdType() }.into();
    #[allow(unused_assignments)]
    let mut dead_key_state: u32 = 0;
    let mut chars = [0u16; 4];
    let mut actual_len: isize = 0;

    for &vk in VIRTUAL_KEYCODE_NUMS {
        let Some(key_code_enum) = cg_keycode_to_keycode(vk) else {
            continue;
        };

        dead_key_state = 0;
        let status = unsafe {
            UCKeyTranslate(
                layout_ptr,
                vk,
                K_UC_KEY_ACTION_DOWN,
                0, // no modifiers
                kbd_type,
                K_UC_NO_DEAD_KEYS,
                &mut dead_key_state,
                chars.len(),
                &mut actual_len,
                chars.as_mut_ptr(),
            )
        };

        if status == 0 && actual_len > 0 {
            let len = usize::try_from(actual_len).unwrap_or(0);
            if len == 0 {
                continue;
            }

            let s = String::from_utf16_lossy(&chars[..len]).to_lowercase();

            keymap.entry(s).or_insert(key_code_enum);
        }
    }

    unsafe {
        rini_skylight_sys::CFRelease(keyboard.cast());
    }

    keymap
}

pub fn keycode_from_char(ch: &str) -> Option<KeyCode> {
    generate_virtual_keymap()
        .get(&ch.to_lowercase())
        .copied()
        .or_else(|| fallback_keycode_from_char(ch))
}

fn fallback_keycode_from_char(ch: &str) -> Option<KeyCode> {
    let mut chars = ch.chars();
    let first = chars.next()?.to_ascii_lowercase();
    if chars.next().is_some() {
        return None;
    }

    use KeyCode::*;

    let code = match first {
        'a' => KeyA,
        'b' => KeyB,
        'c' => KeyC,
        'd' => KeyD,
        'e' => KeyE,
        'f' => KeyF,
        'g' => KeyG,
        'h' => KeyH,
        'i' => KeyI,
        'j' => KeyJ,
        'k' => KeyK,
        'l' => KeyL,
        'm' => KeyM,
        'n' => KeyN,
        'o' => KeyO,
        'p' => KeyP,
        'q' => KeyQ,
        'r' => KeyR,
        's' => KeyS,
        't' => KeyT,
        'u' => KeyU,
        'v' => KeyV,
        'w' => KeyW,
        'x' => KeyX,
        'y' => KeyY,
        'z' => KeyZ,
        '0' => Digit0,
        '1' => Digit1,
        '2' => Digit2,
        '3' => Digit3,
        '4' => Digit4,
        '5' => Digit5,
        '6' => Digit6,
        '7' => Digit7,
        '8' => Digit8,
        '9' => Digit9,
        _ => return None,
    };
    Some(code)
}

impl<'de> serde::de::Deserialize<'de> for HotkeySpec {
    fn deserialize<D>(deserializer: D) -> Result<HotkeySpec, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(serde::Deserialize)]
        #[serde(untagged)]
        enum HotkeyRepr {
            Str(String),
            Map {
                modifiers: Option<Modifiers>,
                key_code: Option<KeyCode>,
            },
        }

        let repr = HotkeyRepr::deserialize(deserializer)?;
        match repr {
            HotkeyRepr::Str(s) => {
                let (mods, key_opt) =
                    parse_mods_and_optional_key(&s).map_err(serde::de::Error::custom)?;
                if let Some(k) = key_opt {
                    Ok(HotkeySpec::Hotkey(Hotkey::new(mods, k)))
                } else if mods != Modifiers::empty() {
                    Ok(HotkeySpec::ModifiersOnly { modifiers: mods })
                } else {
                    Err(serde::de::Error::custom(format!(
                        "No key specified in hotkey: {}",
                        s
                    )))
                }
            }
            HotkeyRepr::Map { modifiers, key_code } => {
                let m = modifiers.unwrap_or(Modifiers::empty());
                if let Some(k) = key_code {
                    Ok(HotkeySpec::Hotkey(Hotkey::new(m, k)))
                } else if m != Modifiers::empty() {
                    Ok(HotkeySpec::ModifiersOnly { modifiers: m })
                } else {
                    Err(serde::de::Error::custom("No key specified in hotkey map"))
                }
            }
        }
    }
}

mod tests {
    #[allow(unused)]
    use super::*;

    #[test]
    fn test_virtual_keymap_generation() {
        let keymap = generate_virtual_keymap();
        assert!(!keymap.is_empty(), "Virtual keymap should not be empty");
        assert!(keymap.len() >= 10, "Expected at least 10 mapped characters");
    }

    #[test]
    fn test_keycode_from_char_basic() {
        let keymap = generate_virtual_keymap();
        if !keymap.is_empty() {
            let first_char = keymap.keys().next().unwrap();
            let result = keycode_from_char(first_char);
            assert!(result.is_some(), "Should find keycode for mapped character");
        }
    }

    #[test]
    fn test_fallback_keycode_from_char_basic() {
        assert_eq!(fallback_keycode_from_char("h"), Some(KeyCode::KeyH));
        assert_eq!(fallback_keycode_from_char("1"), Some(KeyCode::Digit1));
        assert_eq!(fallback_keycode_from_char("Z"), Some(KeyCode::KeyZ));
    }

    #[test]
    fn test_from_str_uses_virtual_keymap() {
        let result = KeyCode::from_str("h");
        assert!(result.is_ok(), "Should parse single character 'h'");
    }

    #[test]
    fn test_named_punctuation_uses_layout_map() {
        assert_eq!(
            KeyCode::from_str("comma").unwrap(),
            keycode_from_char(",").unwrap_or(KeyCode::Comma)
        );
        assert_eq!(
            KeyCode::from_str("period").unwrap(),
            keycode_from_char(".").unwrap_or(KeyCode::Period)
        );
        assert_eq!(
            KeyCode::from_str("slash").unwrap(),
            keycode_from_char("/").unwrap_or(KeyCode::Slash)
        );
    }

    #[test]
    fn modifier_key_activity_distinguishes_left_and_right_alt() {
        let left_alt = CGEventFlags::from_bits_retain(
            CGEventFlags::MaskAlternate.bits() | MOD_FAMILIES[1].left_mask,
        );
        let right_alt = CGEventFlags::from_bits_retain(
            CGEventFlags::MaskAlternate.bits() | MOD_FAMILIES[1].right_mask,
        );

        assert!(modifier_key_is_active(left_alt, KeyCode::AltLeft));
        assert!(!modifier_key_is_active(left_alt, KeyCode::AltRight));
        assert!(modifier_key_is_active(right_alt, KeyCode::AltRight));
        assert!(!modifier_key_is_active(right_alt, KeyCode::AltLeft));
    }

    #[test]
    fn modifier_recovery_preserves_right_alt_from_flags() {
        let right_alt = CGEventFlags::from_bits_retain(
            CGEventFlags::MaskAlternate.bits() | MOD_FAMILIES[1].right_mask,
        );
        let pressed_keys = std::collections::HashSet::new();

        let modifiers = modifiers_from_flags_with_keys(right_alt, &pressed_keys);

        assert!(modifiers.contains(Modifiers::ALT_RIGHT));
        assert!(!modifiers.contains(Modifiers::ALT_LEFT));
    }
    #[test]
    fn a_binding_string_parses_modifiers_in_any_order_and_spelling() {
        let a: Hotkey = "Alt + Shift + H".parse().unwrap();
        let b: Hotkey = "shift+option + h".parse().unwrap();
        assert_eq!(a, b);
        let mut alt_shift = Modifiers::ALT;
        alt_shift.insert(Modifiers::SHIFT);
        assert_eq!(a.modifiers, alt_shift);
        assert!("Alt + Shift".parse::<Hotkey>().is_err(), "a hotkey needs a key");
        assert!("Alt + NoSuchKey".parse::<Hotkey>().is_err());
    }

    #[test]
    fn side_prefixes_and_suffixes_pick_one_key_of_the_pair() {
        let left: Hotkey = "LAlt + H".parse().unwrap();
        let right: Hotkey = "Right Alt + H".parse().unwrap();
        let suffix: Hotkey = "AltRight + H".parse().unwrap();
        assert_eq!(left.modifiers, Modifiers::ALT_LEFT);
        assert_eq!(right.modifiers, Modifiers::ALT_RIGHT);
        assert_eq!(suffix.modifiers, Modifiers::ALT_RIGHT);
    }

    #[test]
    fn display_round_trips_through_from_str() {
        for text in ["Alt + H", "LAlt + Shift + H", "RCtrl + Cmd + Space"] {
            let hotkey: Hotkey = text.parse().unwrap();
            let again: Hotkey = hotkey.to_string().parse().unwrap();
            assert_eq!(again, hotkey, "{text} -> {hotkey}");
        }
    }

    #[test]
    fn a_generic_modifier_expands_to_left_right_and_both() {
        assert_eq!(Modifiers::ALT.expand_to_specific().len(), 3);
        assert_eq!(
            Modifiers::ALT_LEFT.expand_to_specific(),
            vec![Modifiers::ALT_LEFT]
        );
        let mut alt_shift = Modifiers::ALT;
        alt_shift.insert(Modifiers::SHIFT);
        assert_eq!(alt_shift.expand_to_specific().len(), 9);
        assert_eq!(Modifiers::empty().expand_to_specific(), vec![Modifiers::empty()]);
    }

    #[test]
    fn a_modifiers_only_spec_gets_the_matching_modifier_key() {
        let spec: HotkeySpec = serde_json::from_str(r#""Alt""#).unwrap();
        assert_eq!(spec, HotkeySpec::ModifiersOnly { modifiers: Modifiers::ALT });
        assert_eq!(spec.to_hotkey().unwrap().key_code, KeyCode::AltLeft);
        let right: HotkeySpec = serde_json::from_str(r#""RAlt""#).unwrap();
        assert_eq!(right.to_hotkey().unwrap().key_code, KeyCode::AltRight);
        let full: HotkeySpec = serde_json::from_str(r#""Alt + H""#).unwrap();
        assert!(matches!(full, HotkeySpec::Hotkey(_)));
        assert!(serde_json::from_str::<HotkeySpec>(r#""""#).is_err());
    }
}

#[cfg(test)]
mod mask_tests {
    use super::*;

    /// `crate::input::domain::key` holds each family's mask as a bit pattern so its table needs no
    /// CoreGraphics. This is what stops a wrong literal going unnoticed.
    #[test]
    fn every_family_mask_is_the_core_graphics_one() {
        let expected = [
            ("Ctrl", CGEventFlags::MaskControl),
            ("Alt", CGEventFlags::MaskAlternate),
            ("Shift", CGEventFlags::MaskShift),
            ("Meta", CGEventFlags::MaskCommand),
        ];
        assert_eq!(
            MOD_FAMILIES.len(),
            expected.len(),
            "a family was added without a mask check"
        );
        for (name, flags) in expected {
            let family = MOD_FAMILIES
                .iter()
                .find(|m| m.name == name)
                .unwrap_or_else(|| panic!("no {name} family"));
            assert_eq!(family.mask, flags.0, "{name}");
        }
    }
}
