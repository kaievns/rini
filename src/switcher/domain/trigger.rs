//! Which bindings hold a switch open.
//!
//! The chord is not configured twice. The user binds `switch_window`, `switch_workspace_window` and
//! `cycle_app_windows` like any other command, and each switcher's session keys are DERIVED from its
//! binding — so there is one place a trigger is written and no way for a second record of it to
//! disagree.
//!
//! Which also decides the hold: a switch stays open while the binding's own modifiers stay down, so
//! `Ctrl + Q` holds on Ctrl and `Meta + Ctrl + Tab` holds on both. Nothing has to be named separately,
//! and a binding with no modifiers correctly yields nothing to hold.

use rini_ipc::protocol::SwitchScope;

use crate::input::domain::binding::{WmCmd, WmCommand};
use crate::input::domain::key::Modifiers;
use crate::input::domain::switch_session::SwitchKeys;

/// The command whose binding opens each switcher. The backward commands are one-shot steps and never
/// triggers: Shift on the forward chord is what steps a held switch backward.
const TRIGGERS: [(WmCmd, SwitchScope); 3] = [
    (WmCmd::SwitchWindow, SwitchScope::Everything),
    (WmCmd::SwitchWorkspaceWindow, SwitchScope::Workspace),
    (WmCmd::CycleAppWindows, SwitchScope::App),
];

/// The keys each switcher's session should answer to, given every binding the user has written. A
/// switcher with no binding, or one the layout cannot resolve, has no entry.
///
/// `parse` turns a spec such as `"Ctrl + Q"` into its modifiers and key; it is the platform's job
/// because which physical key a name means depends on the keyboard layout.
///
/// The FIRST binding of each command wins. Two bindings for one command is a config the user can
/// write, and picking one deterministically beats either refusing to hold at all or letting the last
/// one silently win.
pub fn switch_keys_from_bindings<F>(
    bindings: &[(String, WmCommand)],
    mut parse: F,
) -> Vec<SwitchKeys>
where
    F: FnMut(&str) -> Option<(Modifiers, crate::input::domain::key::KeyCode)>,
{
    TRIGGERS
        .iter()
        .filter_map(|(trigger, scope)| {
            let spec = bindings.iter().find_map(|(spec, command)| {
                matches!(command, WmCommand::Wm(cmd) if cmd == trigger).then_some(spec.as_str())
            })?;
            let (modifiers, key) = parse(spec)?;
            Some(SwitchKeys {
                scope: *scope,
                forward: key,
                hold: modifiers,
                // Shift steps the other way. Not configurable: it is the same convention every
                // switcher uses, and a second binding for backward already exists for anyone who wants
                // their own key.
                backward: Modifiers::SHIFT,
            })
        })
        .collect()
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

        let keys = only(switch_keys_from_bindings(&bindings, parse));

        assert_eq!(keys.forward, KeyCode::KeyQ);
        assert_eq!(keys.hold, Modifiers::CONTROL);
        assert_eq!(keys.scope, SwitchScope::Everything);
        assert!(keys.can_hold());
    }

    #[test]
    fn a_different_chord_holds_on_its_own_modifiers() {
        let bindings = vec![binding("Meta + Tab", WmCmd::SwitchWindow)];

        let keys = only(switch_keys_from_bindings(&bindings, parse));

        assert_eq!(keys.forward, KeyCode::Tab);
        assert_eq!(keys.hold, Modifiers::META);
    }

    /// A binding with no modifiers has nothing whose release could commit. It still parses — the
    /// one-shot command works — but it can never hold a session open, and `can_hold` is what says so.
    #[test]
    fn a_bare_key_yields_nothing_to_hold() {
        let bindings = vec![binding("F13", WmCmd::SwitchWindow)];

        let keys = only(switch_keys_from_bindings(&bindings, parse));

        assert_eq!(keys.hold, Modifiers::empty());
        assert!(!keys.can_hold());
    }

    #[test]
    fn no_switch_binding_means_no_session() {
        let bindings = vec![binding("Ctrl + Q", WmCmd::CloseWindow)];
        assert!(switch_keys_from_bindings(&bindings, parse).is_empty());
    }

    /// A spec the current keyboard layout cannot resolve is not a session either. Guessing would bind
    /// the switch to whatever key happened to be at that position.
    #[test]
    fn a_spec_that_does_not_resolve_means_no_session() {
        let bindings = vec![binding("Hyper + Nonsense", WmCmd::SwitchWindow)];
        assert!(switch_keys_from_bindings(&bindings, parse).is_empty());
    }

    /// Two bindings for one command is a config the user can write. Picking the first deterministically
    /// beats refusing to hold at all.
    #[test]
    fn the_first_binding_wins() {
        let bindings = vec![
            binding("Ctrl + Q", WmCmd::SwitchWindow),
            binding("Meta + Tab", WmCmd::SwitchWindow),
        ];

        let keys = only(switch_keys_from_bindings(&bindings, parse));

        assert_eq!(keys.forward, KeyCode::KeyQ);
    }

    /// The backward binding is a separate command and does not become the trigger.
    #[test]
    fn the_backward_binding_is_not_the_trigger() {
        let bindings = vec![
            binding("Ctrl + Q", WmCmd::SwitchWindowBackward),
            binding("Ctrl + Q", WmCmd::SwitchWorkspaceWindowBackward),
            binding("Ctrl + Q", WmCmd::CycleAppWindowsBackward),
        ];
        assert!(switch_keys_from_bindings(&bindings, parse).is_empty());
    }

    /// Each switcher with a binding gets its own keys, carrying its own scope.
    #[test]
    fn every_bound_switcher_gets_a_trigger() {
        let bindings = vec![
            binding("Ctrl + Q", WmCmd::SwitchWindow),
            binding("Meta + Tab", WmCmd::SwitchWorkspaceWindow),
            binding("F13", WmCmd::CycleAppWindows),
        ];

        let keys = switch_keys_from_bindings(&bindings, parse);

        assert_eq!(
            keys.iter().map(|k| (k.scope, k.forward)).collect::<Vec<_>>(),
            vec![
                (SwitchScope::Everything, KeyCode::KeyQ),
                (SwitchScope::Workspace, KeyCode::Tab),
                (SwitchScope::App, KeyCode::F13),
            ]
        );
    }

    fn only(keys: Vec<SwitchKeys>) -> SwitchKeys {
        assert_eq!(keys.len(), 1, "{keys:?}");
        keys.into_iter().next().expect("one")
    }
}
