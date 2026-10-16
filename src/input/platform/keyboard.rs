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
use objc2_core_foundation::{CFData, CFRetained, CFString, CFType};
use objc2_core_graphics::{CGEvent, CGEventField};
use parking_lot::Mutex;

use crate::input::domain::key::{F_KEYS, Hotkey, KeyCode, Modifiers, normalize_token};

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
        if let Some(rest) = t.strip_prefix('f')
            && let Ok(n) = rest.parse::<u8>()
            && (1..=20).contains(&n)
        {
            return Ok(F_KEYS[(n - 1) as usize]);
        }

        let key = match t.as_str() {
            "left" | "arrowleft" => KeyCode::ArrowLeft,
            "right" | "arrowright" => KeyCode::ArrowRight,
            "up" | "arrowup" => KeyCode::ArrowUp,
            "down" | "arrowdown" => KeyCode::ArrowDown,

            "tab" => KeyCode::Tab,
            "space" => KeyCode::Space,
            "backspace" => KeyCode::Backspace,
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

/// The modifiers a spec names and its one key, if it has one. A second key is an error, never a
/// silent choice between `Ctrl + J + K`'s two.
fn parse_mods_and_optional_key(s: &str) -> Result<(Modifiers, Option<KeyCode>), anyhow::Error> {
    let parts: Vec<&str> = s.split('+').map(|p| p.trim()).filter(|p| !p.is_empty()).collect();

    let mut mods = Modifiers::empty();
    let mut key_opt: Option<(&str, KeyCode)> = None;

    for part in parts {
        if mods.insert_from_token(part) {
            continue;
        }
        let code = KeyCode::from_str(part)?;
        if let Some((first, _)) = key_opt {
            return Err(anyhow!(
                "`{s}` names two keys, `{first}` and `{part}`; a binding has one"
            ));
        }
        key_opt = Some((part, code));
    }

    Ok((mods, key_opt.map(|(_, code)| code)))
}

impl FromStr for Hotkey {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (mods, key_opt) = parse_mods_and_optional_key(s)?;
        let key_code = key_opt.ok_or_else(|| anyhow!("No key specified in hotkey: {}", s))?;
        Ok(Hotkey::new(mods, key_code))
    }
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

/// The virtual key codes of HIToolbox's `Events.h` (`kVK_*`) and the key each one is.
const CG_KEYCODES: [(u16, KeyCode); 121] = [
    (0x00, KeyCode::KeyA),
    (0x01, KeyCode::KeyS),
    (0x02, KeyCode::KeyD),
    (0x03, KeyCode::KeyF),
    (0x04, KeyCode::KeyH),
    (0x05, KeyCode::KeyG),
    (0x06, KeyCode::KeyZ),
    (0x07, KeyCode::KeyX),
    (0x08, KeyCode::KeyC),
    (0x09, KeyCode::KeyV),
    (0x0A, KeyCode::IntlBackslash),
    (0x0B, KeyCode::KeyB),
    (0x0C, KeyCode::KeyQ),
    (0x0D, KeyCode::KeyW),
    (0x0E, KeyCode::KeyE),
    (0x0F, KeyCode::KeyR),
    (0x10, KeyCode::KeyY),
    (0x11, KeyCode::KeyT),
    (0x12, KeyCode::Digit1),
    (0x13, KeyCode::Digit2),
    (0x14, KeyCode::Digit3),
    (0x15, KeyCode::Digit4),
    (0x16, KeyCode::Digit6),
    (0x17, KeyCode::Digit5),
    (0x18, KeyCode::Equal),
    (0x19, KeyCode::Digit9),
    (0x1A, KeyCode::Digit7),
    (0x1B, KeyCode::Minus),
    (0x1C, KeyCode::Digit8),
    (0x1D, KeyCode::Digit0),
    (0x1E, KeyCode::BracketRight),
    (0x1F, KeyCode::KeyO),
    (0x20, KeyCode::KeyU),
    (0x21, KeyCode::BracketLeft),
    (0x22, KeyCode::KeyI),
    (0x23, KeyCode::KeyP),
    (0x24, KeyCode::Enter),
    (0x25, KeyCode::KeyL),
    (0x26, KeyCode::KeyJ),
    (0x27, KeyCode::Quote),
    (0x28, KeyCode::KeyK),
    (0x29, KeyCode::Semicolon),
    (0x2A, KeyCode::Backslash),
    (0x2B, KeyCode::Comma),
    (0x2C, KeyCode::Slash),
    (0x2D, KeyCode::KeyN),
    (0x2E, KeyCode::KeyM),
    (0x2F, KeyCode::Period),
    (0x30, KeyCode::Tab),
    (0x31, KeyCode::Space),
    (0x32, KeyCode::Backquote),
    (0x33, KeyCode::Backspace),
    (0x34, KeyCode::NumpadEnter),
    (0x35, KeyCode::Escape),
    (0x36, KeyCode::MetaRight),
    (0x37, KeyCode::MetaLeft),
    (0x38, KeyCode::ShiftLeft),
    (0x39, KeyCode::CapsLock),
    (0x3A, KeyCode::AltLeft),
    (0x3B, KeyCode::ControlLeft),
    (0x3C, KeyCode::ShiftRight),
    (0x3D, KeyCode::AltRight),
    (0x3E, KeyCode::ControlRight),
    (0x3F, KeyCode::Fn),
    (0x40, KeyCode::F17),
    (0x41, KeyCode::NumpadDecimal),
    (0x43, KeyCode::NumpadMultiply),
    (0x45, KeyCode::NumpadAdd),
    (0x47, KeyCode::NumLock),
    (0x48, KeyCode::AudioVolumeUp),
    (0x49, KeyCode::AudioVolumeDown),
    (0x4A, KeyCode::AudioVolumeMute),
    (0x4B, KeyCode::NumpadDivide),
    (0x4C, KeyCode::NumpadEnter),
    (0x4E, KeyCode::NumpadSubtract),
    (0x4F, KeyCode::F18),
    (0x50, KeyCode::F19),
    (0x51, KeyCode::NumpadEqual),
    (0x52, KeyCode::Numpad0),
    (0x53, KeyCode::Numpad1),
    (0x54, KeyCode::Numpad2),
    (0x55, KeyCode::Numpad3),
    (0x56, KeyCode::Numpad4),
    (0x57, KeyCode::Numpad5),
    (0x58, KeyCode::Numpad6),
    (0x59, KeyCode::Numpad7),
    (0x5A, KeyCode::F20),
    (0x5B, KeyCode::Numpad8),
    (0x5C, KeyCode::Numpad9),
    (0x5D, KeyCode::IntlYen),
    (0x5E, KeyCode::IntlRo),
    (0x5F, KeyCode::NumpadComma),
    (0x60, KeyCode::F5),
    (0x61, KeyCode::F6),
    (0x62, KeyCode::F7),
    (0x63, KeyCode::F3),
    (0x64, KeyCode::F8),
    (0x65, KeyCode::F9),
    (0x66, KeyCode::Lang2),
    (0x67, KeyCode::F11),
    (0x68, KeyCode::Lang1),
    (0x69, KeyCode::F13),
    (0x6A, KeyCode::F16),
    (0x6B, KeyCode::F14),
    (0x6D, KeyCode::F10),
    (0x6E, KeyCode::ContextMenu),
    (0x6F, KeyCode::F12),
    (0x71, KeyCode::F15),
    (0x72, KeyCode::Insert),
    (0x73, KeyCode::Home),
    (0x74, KeyCode::PageUp),
    (0x75, KeyCode::Delete),
    (0x76, KeyCode::F4),
    (0x77, KeyCode::End),
    (0x78, KeyCode::F2),
    (0x79, KeyCode::PageDown),
    (0x7A, KeyCode::F1),
    (0x7B, KeyCode::ArrowLeft),
    (0x7C, KeyCode::ArrowRight),
    (0x7D, KeyCode::ArrowDown),
    (0x7E, KeyCode::ArrowUp),
];

const fn build_cg_keycode_table() -> [Option<KeyCode>; 0x80] {
    let mut t: [Option<KeyCode>; 0x80] = [None; 0x80];
    let mut i = 0;
    while i < CG_KEYCODES.len() {
        let (code, key) = CG_KEYCODES[i];
        t[code as usize] = Some(key);
        i += 1;
    }
    t
}

const CG_KEYCODE_TABLE: [Option<KeyCode>; 0x80] = build_cg_keycode_table();

#[link(name = "Carbon", kind = "framework")]
unsafe extern "C" {
    /// Takes nothing, and answers an input source rini owns under the Copy rule, or null.
    safe fn TISCopyCurrentASCIICapableKeyboardLayoutInputSource() -> Option<NonNull<CFType>>;
    fn TISGetInputSourceProperty(keyboard: &CFType, property: &CFString) -> *mut c_void;
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
    /// Takes nothing and reads the keyboard's hardware type.
    safe fn LMGetKbdType() -> u8;
    /// An immutable `CFString` constant the Text Input Sources framework exports.
    safe static kTISPropertyUnicodeKeyLayoutData: &'static CFString;
}

const VIRTUAL_KEYCODE_NUMS: &[u16] = &[
    0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0A, 0x0B, 0x0C, 0x0D, 0x0E, 0x0F,
    0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1A, 0x1B, 0x1C, 0x1D, 0x1E, 0x1F,
    0x20, 0x21, 0x22, 0x23, 0x25, 0x26, 0x27, 0x28, 0x29, 0x2A, 0x2B, 0x2C, 0x2D, 0x2E, 0x2F,
    0x32, // backquote
    // keypad subset
    0x41, 0x43, 0x45, 0x47, 0x4B, 0x4C, 0x4E, 0x51, 0x52, 0x53, 0x54, 0x55, 0x56, 0x57, 0x58, 0x59,
    0x5B, 0x5C,
];

fn generate_virtual_keymap() -> StdHashMap<String, KeyCode> {
    static KEYMAP_GENERATION_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));
    let _guard = KEYMAP_GENERATION_LOCK.lock();
    let mut keymap = StdHashMap::new();

    let Some(keyboard) = TISCopyCurrentASCIICapableKeyboardLayoutInputSource() else {
        tracing::warn!("Could not get ASCII-capable keyboard layout input source");
        return keymap;
    };
    // SAFETY: a Copy-rule input source is a +1 reference this function owns, released on drop.
    let keyboard: CFRetained<CFType> = unsafe { CFRetained::from_raw(keyboard) };

    // SAFETY: `keyboard` is a live input source and the key is the framework's own. Under the Get
    // rule the answer is borrowed from `keyboard`, which outlives every use of it below.
    let layout_data =
        unsafe { TISGetInputSourceProperty(&keyboard, kTISPropertyUnicodeKeyLayoutData) };
    // SAFETY: a non-null property is a live CF object, borrowed from `keyboard`.
    let layout_data = unsafe { layout_data.cast::<CFType>().as_ref() }
        .and_then(|property| property.downcast_ref::<CFData>());
    let Some(layout_data) = layout_data else {
        tracing::warn!("Could not get keyboard layout data");
        return keymap;
    };

    let layout_ptr = layout_data.byte_ptr();

    const K_UC_KEY_ACTION_DOWN: u16 = 0;
    const K_UC_NO_DEAD_KEYS: u32 = 1;

    let kbd_type: u32 = LMGetKbdType().into();
    let mut chars = [0u16; 4];
    let mut actual_len: isize = 0;

    for &vk in VIRTUAL_KEYCODE_NUMS {
        let Some(key_code_enum) = cg_keycode_to_keycode(vk) else {
            continue;
        };

        let mut dead_key_state: u32 = 0;
        // SAFETY: `layout_ptr` is the layout's bytes, alive while `keyboard` is; `chars` is passed
        // with its own length, and both out-parameters are live locals.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_virtual_key_code_is_the_key_its_table_row_names_and_an_unlisted_one_is_none() {
        for (code, key) in CG_KEYCODES {
            assert_eq!(cg_keycode_to_keycode(code), Some(key), "{code:#04x}");
        }
        assert_eq!(cg_keycode_to_keycode(0x00), Some(KeyCode::KeyA));
        assert_eq!(cg_keycode_to_keycode(0x34), Some(KeyCode::NumpadEnter));
        assert_eq!(cg_keycode_to_keycode(0x4C), Some(KeyCode::NumpadEnter));
        assert_eq!(cg_keycode_to_keycode(0x7E), Some(KeyCode::ArrowUp));
        for unlisted in [0x42, 0x44, 0x46, 0x4D, 0x6C, 0x70, 0x7F, 0x80, 0xFFFF] {
            assert_eq!(cg_keycode_to_keycode(unlisted), None, "{unlisted:#04x}");
        }
        let listed = (0..0x80u16).filter(|&c| cg_keycode_to_keycode(c).is_some()).count();
        assert_eq!(listed, CG_KEYCODES.len());
    }

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
        assert_eq!(Modifiers::ALT_LEFT.expand_to_specific(), vec![
            Modifiers::ALT_LEFT
        ]);
        let mut alt_shift = Modifiers::ALT;
        alt_shift.insert(Modifiers::SHIFT);
        assert_eq!(alt_shift.expand_to_specific().len(), 9);
        assert_eq!(Modifiers::empty().expand_to_specific(), vec![Modifiers::empty()]);
    }

    #[test]
    fn a_spec_naming_two_keys_is_refused() {
        let error = "Ctrl+J+K".parse::<Hotkey>().unwrap_err().to_string();
        assert!(error.contains("two keys"), "{error}");
    }

    /// Through `normalize_spec`, as a bind's key reaches `Hotkey::from_str`.
    #[test]
    fn niri_key_names_resolve_to_macos_keys() {
        for (name, key) in [
            ("BackSpace", KeyCode::Backspace),
            ("Return", KeyCode::Enter),
            ("Page_Up", KeyCode::PageUp),
            ("Page_Down", KeyCode::PageDown),
            ("Escape", KeyCode::Escape),
            ("space", KeyCode::Space),
            ("F19", KeyCode::F19),
        ] {
            let spec = crate::input::domain::key::normalize_spec(&format!("Alt+{name}"));
            let hotkey = Hotkey::from_str(&spec).unwrap_or_else(|error| panic!("{name}: {error}"));
            assert_eq!(hotkey.key_code, key, "{name}");
        }
    }
}

#[cfg(test)]
mod mask_tests {
    use objc2_core_graphics::CGEventFlags;

    use crate::input::domain::key::MOD_FAMILIES;

    /// `crate::input::domain::key` holds each family's mask as a bit pattern so its table needs no
    /// CoreGraphics. This is what stops a wrong literal going unnoticed.
    #[test]
    fn every_family_mask_is_the_core_graphics_one() {
        let expected = [
            ("Ctrl", CGEventFlags::MaskControl),
            ("Alt", CGEventFlags::MaskAlternate),
            ("Shift", CGEventFlags::MaskShift),
            ("Super", CGEventFlags::MaskCommand),
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
