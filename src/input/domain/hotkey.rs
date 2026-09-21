//! Whether the keys held down are the ones a binding asked for.
//!
//! The types themselves still live in `crate::input::key`, which has not been split yet; what is
//! here is the matching, which needs no keyboard.

use crate::input::key::Modifiers;

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
    SIDES.iter().all(|&(left, right)| {
        match (target.contains(left), target.contains(right)) {
            (true, true) => active.contains(left) || active.contains(right),
            (true, false) => active.contains(left),
            (false, true) => active.contains(right),
            (false, false) => true,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let down = held(&[Modifiers::CONTROL_LEFT, Modifiers::SHIFT_LEFT, Modifiers::META_RIGHT]);
        assert!(modifiers_satisfy(target, down), "extra modifiers do not disqualify");
    }

    #[test]
    fn every_family_the_binding_names_has_to_be_held() {
        let target = held(&[Modifiers::CONTROL_LEFT, Modifiers::SHIFT_RIGHT]);
        assert!(modifiers_satisfy(target, held(&[Modifiers::CONTROL_LEFT, Modifiers::SHIFT_RIGHT])));
        assert!(!modifiers_satisfy(target, Modifiers::CONTROL_LEFT), "shift missing");
        assert!(!modifiers_satisfy(target, Modifiers::SHIFT_RIGHT), "control missing");
    }

    #[test]
    fn a_binding_naming_nothing_is_always_satisfied() {
        assert!(modifiers_satisfy(Modifiers::empty(), Modifiers::empty()));
        assert!(modifiers_satisfy(Modifiers::empty(), Modifiers::META_LEFT));
    }
}
