//! Which binding holds a switch open.
//!
//! The chord is not configured twice. The user binds `switch_window` like any other command, and the
//! session's keys are DERIVED from that binding — so there is one place the trigger is written and no
//! way for a second record of it to disagree.
//!
//! Which also decides the hold: a switch stays open while the binding's own modifiers stay down, so
//! `Ctrl + Q` holds on Ctrl and `Meta + Ctrl + Tab` holds on both. Nothing has to be named separately,
//! and a binding with no modifiers correctly yields nothing to hold.

use crate::input::domain::binding::{WmCmd, WmCommand};
use crate::input::domain::key::Modifiers;
use crate::input::domain::switch_session::SwitchKeys;

/// The keys a switcher session should answer to, given every binding the user has written.
///
/// `parse` turns a spec such as `"Ctrl + Q"` into its modifiers and key; it is the platform's job
/// because which physical key a name means depends on the keyboard layout.
///
/// The FIRST binding of `switch_window` wins. Two bindings for one command is a config the user can
/// write, and picking one deterministically beats either refusing to hold at all or letting the last
/// one silently win.
pub fn switch_keys_from_bindings<F>(
    bindings: &[(String, WmCommand)],
    mut parse: F,
) -> Option<SwitchKeys>
where
    F: FnMut(&str) -> Option<(Modifiers, crate::input::domain::key::KeyCode)>,
{
    let spec = bindings.iter().find_map(|(spec, command)| {
        matches!(command, WmCommand::Wm(WmCmd::SwitchWindow)).then_some(spec.as_str())
    })?;
    let (modifiers, key) = parse(spec)?;
    Some(SwitchKeys {
        forward: key,
        hold: modifiers,
        // Shift steps the other way. Not configurable: it is the same convention every switcher uses,
        // and a second binding for backward already exists for anyone who wants their own key.
        backward: Modifiers::SHIFT,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::domain::key::KeyCode;

    fn parse(spec: &str) -> Option<(Modifiers, KeyCode)> {
        match spec {
            "Ctrl + Q" => Some((Modifiers::CONTROL, KeyCode::KeyQ)),
            "Meta + Tab" => Some((Modifiers::META, KeyCode::Tab)),
            "F13" => Some((Modifiers::empty(), KeyCode::F13)),
            _ => None,
        }
    }

    fn binding(spec: &str, cmd: WmCmd) -> (String, WmCommand) {
        (spec.to_owned(), WmCommand::Wm(cmd))
    }

    /// The hold comes from the binding's own modifiers, so nothing is configured twice.
    #[test]
    fn the_binding_decides_both_the_key_and_what_holds_it() {
        let bindings = vec![binding("Ctrl + Q", WmCmd::SwitchWindow)];

        let keys = switch_keys_from_bindings(&bindings, parse).expect("keys");

        assert_eq!(keys.forward, KeyCode::KeyQ);
        assert_eq!(keys.hold, Modifiers::CONTROL);
        assert!(keys.can_hold());
    }

    #[test]
    fn a_different_chord_holds_on_its_own_modifiers() {
        let bindings = vec![binding("Meta + Tab", WmCmd::SwitchWindow)];

        let keys = switch_keys_from_bindings(&bindings, parse).expect("keys");

        assert_eq!(keys.forward, KeyCode::Tab);
        assert_eq!(keys.hold, Modifiers::META);
    }

    /// A binding with no modifiers has nothing whose release could commit. It still parses — the
    /// one-shot command works — but it can never hold a session open, and `can_hold` is what says so.
    #[test]
    fn a_bare_key_yields_nothing_to_hold() {
        let bindings = vec![binding("F13", WmCmd::SwitchWindow)];

        let keys = switch_keys_from_bindings(&bindings, parse).expect("keys");

        assert_eq!(keys.hold, Modifiers::empty());
        assert!(!keys.can_hold());
    }

    #[test]
    fn no_switch_binding_means_no_session() {
        let bindings = vec![binding("Ctrl + Q", WmCmd::CloseWindow)];
        assert_eq!(switch_keys_from_bindings(&bindings, parse), None);
    }

    /// A spec the current keyboard layout cannot resolve is not a session either. Guessing would bind
    /// the switch to whatever key happened to be at that position.
    #[test]
    fn a_spec_that_does_not_resolve_means_no_session() {
        let bindings = vec![binding("Hyper + Nonsense", WmCmd::SwitchWindow)];
        assert_eq!(switch_keys_from_bindings(&bindings, parse), None);
    }

    /// Two bindings for one command is a config the user can write. Picking the first deterministically
    /// beats refusing to hold at all.
    #[test]
    fn the_first_binding_wins() {
        let bindings = vec![
            binding("Ctrl + Q", WmCmd::SwitchWindow),
            binding("Meta + Tab", WmCmd::SwitchWindow),
        ];

        let keys = switch_keys_from_bindings(&bindings, parse).expect("keys");

        assert_eq!(keys.forward, KeyCode::KeyQ);
    }

    /// The backward binding is a separate command and does not become the trigger.
    #[test]
    fn the_backward_binding_is_not_the_trigger() {
        let bindings = vec![binding("Ctrl + Q", WmCmd::SwitchWindowBackward)];
        assert_eq!(switch_keys_from_bindings(&bindings, parse), None);
    }
}
