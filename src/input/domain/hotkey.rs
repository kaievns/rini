//! Whether the keys held down are the ones a binding asked for, and what a press of one fires.
//!
//! The matching, which needs no keyboard. Which physical key a spec names does, and is
//! `crate::input::platform::keyboard`.

use rustc_hash::FxHashMap as HashMap;

use crate::input::domain::binding::{Action, Bind};
use crate::input::domain::key::{Hotkey, Modifiers};

/// Every bound key, by each exact chord it answers to.
#[derive(Debug, Default)]
pub struct HotkeyTable {
    binds: HashMap<Hotkey, (Action, bool)>,
}

impl HotkeyTable {
    /// The table for `binds`, each key already free of `Mod`. `resolve` names the physical key, which
    /// depends on the keyboard layout; a key it cannot resolve is left out and returned.
    pub fn build(binds: &[Bind], resolve: impl Fn(&str) -> Option<Hotkey>) -> (Self, Vec<String>) {
        let mut table = HotkeyTable::default();
        let mut unresolved = Vec::new();
        for bind in binds {
            let Some(hotkey) = resolve(&bind.key) else {
                unresolved.push(bind.key.clone());
                continue;
            };
            // A generic modifier is either side or both, and macOS reports the side.
            for modifiers in hotkey.modifiers.expand_to_specific() {
                table
                    .binds
                    .entry(Hotkey::new(modifiers, hotkey.key_code))
                    .or_insert_with(|| (bind.action.clone(), bind.repeat));
            }
        }
        (table, unresolved)
    }

    pub fn is_empty(&self) -> bool { self.binds.is_empty() }

    pub fn len(&self) -> usize { self.binds.len() }

    /// What a key-down of `hotkey` fires: `None` when nothing is bound to it, so the key goes on to
    /// the application. A held key's repeats fire only a bind written `repeat=true`; otherwise they
    /// are swallowed, because a repeat re-fired commands such as workspace back-and-forth.
    pub fn press(&self, hotkey: &Hotkey, is_repeat: bool) -> Option<Option<Action>> {
        let (action, repeat) = self.binds.get(hotkey)?;
        Some((!is_repeat || *repeat).then(|| action.clone()))
    }
}

/// Every modifier family, as the pair of side-specific flags that name it.
const SIDES: [(Modifiers, Modifiers); 4] = [
    (Modifiers::SHIFT_LEFT, Modifiers::SHIFT_RIGHT),
    (Modifiers::CONTROL_LEFT, Modifiers::CONTROL_RIGHT),
    (Modifiers::ALT_LEFT, Modifiers::ALT_RIGHT),
    (Modifiers::META_LEFT, Modifiers::META_RIGHT),
];

/// Whether the modifiers currently held satisfy what a binding asks for.
///
/// Per family, three cases. A binding naming both sides — which is what a side-agnostic `alt` parses
/// to — is satisfied by either one. A binding naming one side requires that side, so `alt_right` does
/// not fire on the left key. A binding naming neither ignores the family, so extra modifiers held
/// alongside do not stop it: this is a "held down?" test, not an exact match.
pub fn modifiers_satisfy(target: Modifiers, active: Modifiers) -> bool {
    SIDES.iter().all(
        |&(left, right)| match (target.contains(left), target.contains(right)) {
            (true, true) => active.contains(left) || active.contains(right),
            (true, false) => active.contains(left),
            (false, true) => active.contains(right),
            (false, false) => true,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::domain::key::KeyCode;

    fn table(binds: &[(&str, Action, bool)]) -> HotkeyTable {
        let binds: Vec<Bind> = binds
            .iter()
            .map(|(key, action, repeat)| Bind {
                key: key.to_string(),
                action: action.clone(),
                repeat: *repeat,
            })
            .collect();
        let (table, unresolved) = HotkeyTable::build(&binds, |key| match key {
            "Alt+J" => Some(Hotkey::new(Modifiers::ALT, KeyCode::KeyJ)),
            "Alt+K" => Some(Hotkey::new(Modifiers::ALT, KeyCode::KeyK)),
            _ => None,
        });
        assert!(unresolved.is_empty());
        table
    }

    fn left_alt(key: KeyCode) -> Hotkey { Hotkey::new(Modifiers::ALT_LEFT, key) }

    /// D3: a held key fires once unless its bind asks for repeats.
    #[test]
    fn a_repeat_fires_only_a_bind_that_asks_for_repeats() {
        let table = table(&[
            ("Alt+J", Action::FocusColumnLeft, false),
            ("Alt+K", Action::FocusColumnRight, true),
        ]);
        assert_eq!(
            table.press(&left_alt(KeyCode::KeyJ), false),
            Some(Some(Action::FocusColumnLeft))
        );
        assert_eq!(
            table.press(&left_alt(KeyCode::KeyJ), true),
            Some(None),
            "swallowed, not fired"
        );
        assert_eq!(
            table.press(&left_alt(KeyCode::KeyK), true),
            Some(Some(Action::FocusColumnRight))
        );
    }

    #[test]
    fn an_unbound_key_goes_on_to_the_application() {
        let table = table(&[("Alt+J", Action::FocusColumnLeft, false)]);
        assert_eq!(table.press(&left_alt(KeyCode::KeyL), false), None);
        assert_eq!(
            table.press(&Hotkey::new(Modifiers::empty(), KeyCode::KeyJ), false),
            None
        );
    }

    /// A generic `Alt` answers to either side and both, three chords for one bind.
    #[test]
    fn a_generic_modifier_answers_to_either_side() {
        let table = table(&[("Alt+J", Action::FocusColumnLeft, false)]);
        assert_eq!(table.len(), 3);
        assert!(table.press(&Hotkey::new(Modifiers::ALT_RIGHT, KeyCode::KeyJ), false).is_some());
        assert!(table.press(&Hotkey::new(Modifiers::ALT, KeyCode::KeyJ), false).is_some());
    }

    #[test]
    fn a_key_the_layout_cannot_resolve_is_left_out_and_named() {
        let binds = vec![Bind {
            key: "Mystery".into(),
            action: Action::Debug,
            repeat: false,
        }];
        let (table, unresolved) = HotkeyTable::build(&binds, |_| None);
        assert!(table.is_empty());
        assert_eq!(unresolved, ["Mystery"]);
    }

    /// `Modifiers` is a hand-rolled newtype without `BitOr`, so a set is built by insertion.
    fn held(parts: &[Modifiers]) -> Modifiers {
        let mut all = Modifiers::empty();
        for part in parts {
            all.insert(*part);
        }
        all
    }

    #[test]
    fn a_binding_naming_both_sides_is_satisfied_by_either() {
        let target = held(&[Modifiers::ALT_LEFT, Modifiers::ALT_RIGHT]);
        assert!(modifiers_satisfy(target, Modifiers::ALT_LEFT));
        assert!(modifiers_satisfy(target, Modifiers::ALT_RIGHT));
        assert!(!modifiers_satisfy(target, Modifiers::SHIFT_LEFT));
    }

    #[test]
    fn a_binding_naming_one_side_is_not_satisfied_by_the_other() {
        assert!(modifiers_satisfy(Modifiers::ALT_RIGHT, Modifiers::ALT_RIGHT));
        assert!(!modifiers_satisfy(Modifiers::ALT_RIGHT, Modifiers::ALT_LEFT));
    }

    #[test]
    fn a_family_the_binding_does_not_name_is_ignored() {
        let target = held(&[Modifiers::CONTROL_LEFT, Modifiers::CONTROL_RIGHT]);
        let down = held(&[
            Modifiers::CONTROL_LEFT,
            Modifiers::SHIFT_LEFT,
            Modifiers::META_RIGHT,
        ]);
        assert!(
            modifiers_satisfy(target, down),
            "extra modifiers do not disqualify"
        );
    }

    #[test]
    fn every_family_the_binding_names_has_to_be_held() {
        let target = held(&[Modifiers::CONTROL_LEFT, Modifiers::SHIFT_RIGHT]);
        assert!(modifiers_satisfy(
            target,
            held(&[Modifiers::CONTROL_LEFT, Modifiers::SHIFT_RIGHT])
        ));
        assert!(
            !modifiers_satisfy(target, Modifiers::CONTROL_LEFT),
            "shift missing"
        );
        assert!(
            !modifiers_satisfy(target, Modifiers::SHIFT_RIGHT),
            "control missing"
        );
    }

    #[test]
    fn a_binding_naming_nothing_is_always_satisfied() {
        assert!(modifiers_satisfy(Modifiers::empty(), Modifiers::empty()));
        assert!(modifiers_satisfy(Modifiers::empty(), Modifiers::META_LEFT));
    }
}
