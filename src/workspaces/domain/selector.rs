//! Which workspace a selector names, and which end of the stack a step runs into.

use rini_ipc::protocol::{RelativeWorkspace, WorkspaceSelector};

use super::virtual_workspace::VirtualWorkspaceId;
use crate::layout::Direction;

/// Workspace names compare ignoring case: the rule the config's duplicate check states, so no
/// reference can tell apart two names that check calls the same.
pub fn same_workspace_name(a: &str, b: &str) -> bool { a.to_lowercase() == b.to_lowercase() }

/// The workspace `selector` names among `workspaces`, which are in stack order.
///
/// An index counts from 0, a name compares by `same_workspace_name`, and a step goes one along the
/// stack from `current` without wrapping. `None` when nothing is named, there is no further
/// workspace that way, or a step has no `current` to start from.
pub fn resolve_workspace(
    selector: &WorkspaceSelector,
    current: Option<VirtualWorkspaceId>,
    workspaces: &[(VirtualWorkspaceId, String)],
) -> Option<VirtualWorkspaceId> {
    let id_at = |index: usize| workspaces.get(index).map(|(id, _)| *id);
    match selector {
        WorkspaceSelector::Index(index) => id_at(*index),
        WorkspaceSelector::Name(name) => workspaces
            .iter()
            .find_map(|(id, workspace)| same_workspace_name(workspace, name).then_some(*id)),
        WorkspaceSelector::Relative(step) => {
            let at = workspaces.iter().position(|(id, _)| Some(*id) == current)?;
            match step {
                RelativeWorkspace::Next => id_at(at + 1),
                RelativeWorkspace::Prev => at.checked_sub(1).and_then(id_at),
            }
        }
    }
}

/// Which end of the stack a step runs into, for anything that has to name the edge it hit. `None`
/// for a selector that names a workspace outright, because failing to find one is not an edge of
/// anything.
///
/// Workspaces stack downward: `Next` is below, `Prev` above. The same orientation
/// `handle_workspace_command` reports when a switch has nowhere to go.
pub fn workspace_stack_direction(selector: &WorkspaceSelector) -> Option<Direction> {
    match selector {
        WorkspaceSelector::Relative(RelativeWorkspace::Next) => Some(Direction::Down),
        WorkspaceSelector::Relative(RelativeWorkspace::Prev) => Some(Direction::Up),
        WorkspaceSelector::Name(_) | WorkspaceSelector::Index(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use slotmap::SlotMap;

    use super::*;

    fn stack(names: [&str; 3]) -> Vec<(VirtualWorkspaceId, String)> {
        let mut ids = SlotMap::<VirtualWorkspaceId, ()>::with_key();
        names.into_iter().map(|name| (ids.insert(()), name.to_owned())).collect()
    }

    #[test]
    fn a_name_matches_ignoring_case_and_nothing_else() {
        assert!(same_workspace_name("Web", "web"));
        assert!(same_workspace_name("ÉCRAN", "écran"));
        assert!(!same_workspace_name("web", "webs"));
    }

    /// One row per selector and starting point: what it names, or nothing.
    #[test]
    fn a_selector_resolves_to_the_workspace_it_names() {
        let workspaces = stack(["Main", "web", "chat"]);
        let [main, web, chat] = [0, 1, 2].map(|i| workspaces[i].0);
        let name = |name: &str| WorkspaceSelector::Name(name.to_owned());
        let next = WorkspaceSelector::Relative(RelativeWorkspace::Next);
        let prev = WorkspaceSelector::Relative(RelativeWorkspace::Prev);
        let rows = [
            (WorkspaceSelector::Index(0), None, Some(main)),
            (WorkspaceSelector::Index(2), Some(main), Some(chat)),
            (WorkspaceSelector::Index(3), None, None),
            (name("web"), None, Some(web)),
            (name("WEB"), None, Some(web)),
            (name("main"), Some(chat), Some(main)),
            (name("nowhere"), Some(main), None),
            (next.clone(), Some(main), Some(web)),
            (next.clone(), Some(web), Some(chat)),
            (next.clone(), Some(chat), None),
            (prev.clone(), Some(chat), Some(web)),
            (prev.clone(), Some(main), None),
            (next, None, None),
            (prev, None, None),
        ];
        for (selector, current, expected) in rows {
            assert_eq!(
                resolve_workspace(&selector, current, &workspaces),
                expected,
                "{selector:?} from {current:?}"
            );
        }
    }

    /// Only the lowercase wire strings are steps, so any other spelling reaches a workspace that
    /// carries a step's name.
    #[test]
    fn a_workspace_called_next_is_reached_by_another_spelling_of_its_name() {
        let workspaces = stack(["main", "next", "prev"]);
        let [_, next, prev] = [0, 1, 2].map(|i| workspaces[i].0);
        for (name, expected) in [("Next", next), ("NEXT", next), ("Prev", prev)] {
            let selector = WorkspaceSelector::Name(name.to_owned());
            assert_eq!(
                resolve_workspace(&selector, None, &workspaces),
                Some(expected),
                "{name}"
            );
        }
    }

    /// Workspaces stack downward, so `Next` is an edge at the BOTTOM and `Prev` one at the top.
    #[test]
    fn a_step_names_the_edge_it_can_run_into() {
        assert_eq!(
            workspace_stack_direction(&WorkspaceSelector::Relative(RelativeWorkspace::Next)),
            Some(Direction::Down)
        );
        assert_eq!(
            workspace_stack_direction(&WorkspaceSelector::Relative(RelativeWorkspace::Prev)),
            Some(Direction::Up)
        );
    }

    /// Asking for workspace 7 when there are four is a request that cannot be honoured, not a push
    /// against the end of the stack. Bouncing for it would say the stack has an edge in a direction
    /// the user never named.
    #[test]
    fn naming_a_workspace_outright_is_not_a_direction() {
        assert_eq!(workspace_stack_direction(&WorkspaceSelector::Index(7)), None);
        assert_eq!(
            workspace_stack_direction(&WorkspaceSelector::Name("scratch".to_owned())),
            None
        );
    }
}
