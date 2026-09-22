//! What a binding alias means, as a translation rather than a side effect.
//!
//! `WmCmd` is the vocabulary a config binding is written in. Most of it is a different name for a
//! `reactor::Command`, and a few entries are things only the application can do: reload the config,
//! run a shell command. Both used to be ninety lines of match arms inside `handle_event`, each arm
//! ending in a `send`, so the translation could not be checked without a running controller.
//!
//! The one arm that is not a pure rename is `SwitchToWorkspace` by NAME, which has to be resolved
//! against the configured names. They are passed in rather than read, which is what makes the whole
//! thing a function.

use crate::app::config::WorkspaceSelector;
use crate::app::reactor;
use crate::input::domain::binding::{ExecCmd, WmCmd};
use crate::workspaces::LayoutCommand;

/// What the controller should do about a binding.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Lowered {
    /// Send this to the reactor.
    Command(reactor::Command),
    /// Run this shell command.
    Exec(ExecCmd),
    /// Re-read the config file.
    ReloadConfig,
    /// The binding named a workspace that does not exist. Nothing happens, and the caller says so.
    UnknownWorkspace(WorkspaceSelector),
}

/// Lower a binding alias to what carries it out.
///
/// `workspace_names` is the configured list, consulted only to turn a name into an index.
pub(crate) fn lower(cmd: WmCmd, workspace_names: &[String]) -> Lowered {
    use reactor::Command::{Layout, Reactor};

    let layout = |command| Lowered::Command(Layout(command));
    match cmd {
        WmCmd::ReloadConfig => Lowered::ReloadConfig,
        WmCmd::Exec(cmd) => Lowered::Exec(cmd),
        WmCmd::ToggleSpaceActivated => {
            Lowered::Command(Reactor(reactor::ReactorCommand::ToggleSpaceActivated))
        }
        // No window server id: a hotkey means the focused window, and the reactor resolves that.
        WmCmd::CloseWindow => {
            Lowered::Command(Reactor(reactor::ReactorCommand::CloseWindow { window_server_id: None }))
        }
        WmCmd::CycleAppWindows => {
            Lowered::Command(Reactor(reactor::ReactorCommand::CycleAppWindows { backward: false }))
        }
        WmCmd::CycleAppWindowsBackward => {
            Lowered::Command(Reactor(reactor::ReactorCommand::CycleAppWindows { backward: true }))
        }
        WmCmd::NextWorkspace => layout(LayoutCommand::NextWorkspace(None)),
        WmCmd::PrevWorkspace => layout(LayoutCommand::PrevWorkspace(None)),
        WmCmd::CreateWorkspace => layout(LayoutCommand::CreateWorkspace),
        WmCmd::SwitchToLastWorkspace => layout(LayoutCommand::SwitchToLastWorkspace),
        WmCmd::MoveWindowToWorkspace(workspace) => layout(LayoutCommand::MoveWindowToWorkspace {
            workspace,
            follow: false,
            window_id: None,
        }),
        WmCmd::SwitchToWorkspace(selector) => match workspace_index(&selector, workspace_names) {
            Some(index) => layout(LayoutCommand::SwitchToWorkspace(index)),
            None => Lowered::UnknownWorkspace(selector),
        },
    }
}

/// An index is taken at face value; a name is looked up in the configured order.
///
/// An index is NOT range-checked here. The configured names and the workspaces that exist are two
/// different lists — a workspace can be created at runtime — so the reactor is the only thing that
/// knows whether an index is reachable, and refusing here would break `switch_to_workspace 5` on a
/// machine whose config names four.
fn workspace_index(selector: &WorkspaceSelector, names: &[String]) -> Option<usize> {
    match selector {
        WorkspaceSelector::Index(index) => Some(*index),
        WorkspaceSelector::Name(name) => names.iter().position(|configured| configured == name),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names() -> Vec<String> {
        ["Main", "Development", "Communication", "Utilities"].map(String::from).to_vec()
    }

    fn lowered(cmd: WmCmd) -> Lowered {
        lower(cmd, &names())
    }

    #[test]
    fn a_workspace_name_becomes_its_position_in_the_configured_order() {
        assert_eq!(
            lowered(WmCmd::SwitchToWorkspace(WorkspaceSelector::Name("Communication".into()))),
            Lowered::Command(reactor::Command::Layout(LayoutCommand::SwitchToWorkspace(2)))
        );
    }

    /// A name that is not configured does nothing, and says which name. It used to warn from inside
    /// the dispatcher, which meant the caller could not tell a refusal from a command that was sent.
    #[test]
    fn an_unconfigured_workspace_name_is_refused_by_name() {
        let selector = WorkspaceSelector::Name("Gaming".into());
        assert_eq!(
            lowered(WmCmd::SwitchToWorkspace(selector.clone())),
            Lowered::UnknownWorkspace(selector)
        );
    }

    /// An index beyond the configured names is still passed through: the names and the workspaces
    /// that exist are different lists, and only the reactor knows the second one.
    #[test]
    fn an_index_is_not_checked_against_the_configured_names() {
        assert_eq!(
            lowered(WmCmd::SwitchToWorkspace(WorkspaceSelector::Index(9))),
            Lowered::Command(reactor::Command::Layout(LayoutCommand::SwitchToWorkspace(9)))
        );
    }

    #[test]
    fn the_two_cycle_directions_differ_only_in_their_flag() {
        assert_eq!(
            lowered(WmCmd::CycleAppWindows),
            Lowered::Command(reactor::Command::Reactor(
                reactor::ReactorCommand::CycleAppWindows { backward: false }
            ))
        );
        assert_eq!(
            lowered(WmCmd::CycleAppWindowsBackward),
            Lowered::Command(reactor::Command::Reactor(
                reactor::ReactorCommand::CycleAppWindows { backward: true }
            ))
        );
    }

    /// Moving a window to a workspace does not follow it. The user asked to send the window away,
    /// not to go with it; `follow` exists for the CLI, which asks explicitly.
    #[test]
    fn a_hotkey_move_to_workspace_does_not_follow_the_window() {
        let Lowered::Command(reactor::Command::Layout(LayoutCommand::MoveWindowToWorkspace {
            follow,
            window_id,
            ..
        })) = lowered(WmCmd::MoveWindowToWorkspace(WorkspaceSelector::Index(1)))
        else {
            panic!("expected a layout move");
        };
        assert!(!follow, "a hotkey move leaves focus where it is");
        assert_eq!(window_id, None, "and acts on the selection");
    }

    #[test]
    fn the_two_application_only_aliases_do_not_become_reactor_commands() {
        assert_eq!(lowered(WmCmd::ReloadConfig), Lowered::ReloadConfig);
        let exec = ExecCmd::String("open -a Safari".to_owned());
        assert_eq!(lowered(WmCmd::Exec(exec.clone())), Lowered::Exec(exec));
    }

    /// Every alias lowers to something distinct. Ninety near-identical arms is where a copy-paste
    /// sends two bindings to one command and nothing notices.
    #[test]
    fn no_two_aliases_lower_to_the_same_thing() {
        let aliases = [
            WmCmd::ToggleSpaceActivated,
            WmCmd::CloseWindow,
            WmCmd::CycleAppWindows,
            WmCmd::CycleAppWindowsBackward,
            WmCmd::NextWorkspace,
            WmCmd::PrevWorkspace,
            WmCmd::CreateWorkspace,
            WmCmd::SwitchToLastWorkspace,
            WmCmd::SwitchToWorkspace(WorkspaceSelector::Index(0)),
            WmCmd::MoveWindowToWorkspace(WorkspaceSelector::Index(0)),
        ];
        let mut seen: Vec<Lowered> = Vec::new();
        for alias in aliases {
            let got = lower(alias.clone(), &names());
            assert!(!seen.contains(&got), "{alias:?} duplicates another alias: {got:?}");
            seen.push(got);
        }
    }
}
