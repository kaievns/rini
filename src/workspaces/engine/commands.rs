//! What a layout command acts on, and one module per family of commands.
//!
//! `handle_command` was 272 lines: a 50-line prelude resolving the space, its active workspace and
//! that workspace's active layout, then seventeen match arms over `LayoutCommand`. The prelude is the
//! same for every arm and three commands are answered before it finishes, which is the part worth
//! naming; the arms are grouped by what they move.

use rini_core::ids::SpaceId;

use crate::layout::LayoutId;
use crate::workspaces::domain::virtual_workspace::VirtualWorkspaceId;

pub(super) mod arrange;
pub(super) mod floating;
pub(super) mod focus;
pub(super) mod resize;
pub(super) mod strip;

/// The space a command acts on, its active workspace, and that workspace's active layout.
///
/// Every arm of `handle_command` needs all three, and resolving them is the one thing the prelude
/// does that can fail: a space rini has not seen has no active workspace, and a workspace that has
/// never been laid out has no active layout. Either means the command does nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct CommandTarget {
    pub(super) space: SpaceId,
    pub(super) workspace: VirtualWorkspaceId,
    pub(super) layout: LayoutId,
}

/// Why a command could not be given a target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Unresolved {
    /// The caller named no space, and a command with no space has nothing to act on.
    NoSpace,
    /// The space has no active workspace, so rini has never seen it.
    NoActiveWorkspace,
    /// The workspace has no active layout, so it has never been laid out at this size.
    NoActiveLayout,
}

impl Unresolved {
    /// What to log. Each case is a different amount of wrong: no space is ordinary — a hotkey pressed
    /// before any snapshot has arrived — while no active workspace or layout for a space rini IS
    /// tracking means something upstream did not set one up.
    pub(super) fn is_worth_warning_about(self) -> bool {
        !matches!(self, Self::NoSpace)
    }
}

/// Resolve the three things a command acts on.
pub(super) fn resolve_target(
    space: Option<SpaceId>,
    active_workspace: impl FnOnce(SpaceId) -> Option<VirtualWorkspaceId>,
    active_layout: impl FnOnce(SpaceId, VirtualWorkspaceId) -> Option<LayoutId>,
) -> Result<CommandTarget, Unresolved> {
    let space = space.ok_or(Unresolved::NoSpace)?;
    let workspace = active_workspace(space).ok_or(Unresolved::NoActiveWorkspace)?;
    let layout = active_layout(space, workspace).ok_or(Unresolved::NoActiveLayout)?;
    Ok(CommandTarget { space, workspace, layout })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn space() -> SpaceId {
        SpaceId::new(1)
    }

    #[test]
    fn a_space_with_a_workspace_and_a_layout_resolves() {
        let workspace = VirtualWorkspaceId::default();
        let layout = LayoutId::default();
        let target = resolve_target(Some(space()), |_| Some(workspace), |_, _| Some(layout))
            .expect("resolves");
        assert_eq!(
            target,
            CommandTarget {
                space: space(),
                workspace,
                layout
            }
        );
    }

    /// Each failure says which of the three was missing, because they mean different things and the
    /// log is the only place the difference shows.
    #[test]
    fn each_missing_piece_is_reported_as_itself() {
        assert_eq!(
            resolve_target(
                None,
                |_| Some(VirtualWorkspaceId::default()),
                |_, _| Some(LayoutId::default())
            ),
            Err(Unresolved::NoSpace)
        );
        assert_eq!(
            resolve_target(Some(space()), |_| None, |_, _| Some(LayoutId::default())),
            Err(Unresolved::NoActiveWorkspace)
        );
        assert_eq!(
            resolve_target(
                Some(space()),
                |_| Some(VirtualWorkspaceId::default()),
                |_, _| None
            ),
            Err(Unresolved::NoActiveLayout)
        );
    }

    /// A hotkey pressed before the first topology snapshot has no space, and that is ordinary. The
    /// other two mean a space rini IS tracking was never set up, which is worth saying out loud.
    #[test]
    fn only_a_missing_space_is_unremarkable() {
        assert!(!Unresolved::NoSpace.is_worth_warning_about());
        assert!(Unresolved::NoActiveWorkspace.is_worth_warning_about());
        assert!(Unresolved::NoActiveLayout.is_worth_warning_about());
    }
}
