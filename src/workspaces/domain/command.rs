//! A layout command as the engine takes it: one family per dispatcher, each its own enum.
//!
//! The wire's `LayoutCommand` is flat because its names are the protocol's. `From` sorts it into
//! families in one exhaustive match, so a new wire command fails to compile until it is given one,
//! and each family's dispatcher matches only its own enum.

use rini_core::ids::WindowId;
use rini_ipc::protocol::{LayoutCommand, WorkspaceSelector};

use super::workspace_focus::Cycle;
use crate::layout::{Direction, ResizeOrientation, StripEnd};

#[derive(Debug, Clone, PartialEq)]
pub enum EngineCommand {
    Focus(FocusCommand),
    Arrange(ArrangeCommand),
    Resize(ResizeCommand),
    Strip(StripCommand),
    Floating(FloatingCommand),
    Workspace(WorkspaceCommand),
}

/// Moving focus along the strip, and across to the next display when it runs out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FocusCommand {
    /// `next_window` / `prev_window`: a step through the visible windows, wrapping at both ends.
    Cycle(Cycle),
    Move(Direction),
    /// To the column at that end of the strip.
    ToEnd(StripEnd),
}

/// Moving windows within the strip.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArrangeCommand {
    Swap(WindowId, WindowId),
    MoveNode(Direction),
    MoveNodeToEnd(StripEnd),
    ToggleFold(Direction),
    ExplodeColumn,
}

/// Changing a column's width or a window's height.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ResizeCommand {
    By {
        amount: f64,
        orientation: ResizeOrientation,
    },
    CyclePresetWidth,
}

/// Moving the strip itself under the viewport.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StripCommand {
    Snap,
    CenterSelection,
}

/// The floating layer, and maximising, which a floating window answers differently.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FloatingCommand {
    ToggleWindowFloating,
    ToggleFocusFloating,
    /// `toggle_fullscreen_within_gaps`: a tiled column fills the tiling area, a floating window the
    /// whole of it.
    Maximize,
}

/// Which workspace a display shows, and moving a window to another one.
#[derive(Debug, Clone, PartialEq)]
pub enum WorkspaceCommand {
    /// `skip_empty`: `Some(true)` steps over workspaces with no windows.
    Next(Option<bool>),
    Prev(Option<bool>),
    SwitchTo(usize),
    SwitchToLast,
    MoveWindow {
        workspace: WorkspaceSelector,
        follow: bool,
        window_id: Option<u32>,
    },
}

impl WorkspaceCommand {
    /// Whether the display ends up showing another workspace: every switch, and a move only when
    /// focus follows the window.
    pub fn switches_workspace(&self) -> bool {
        match self {
            Self::Next(_) | Self::Prev(_) | Self::SwitchTo(_) | Self::SwitchToLast => true,
            Self::MoveWindow { follow, .. } => *follow,
        }
    }
}

impl EngineCommand {
    /// The workspace command this is, if it is one. The reactor owns which workspace is active, so it
    /// treats these apart from the rest.
    pub fn workspace(&self) -> Option<&WorkspaceCommand> {
        let Self::Workspace(command) = self else {
            return None;
        };
        Some(command)
    }
}

impl From<LayoutCommand> for EngineCommand {
    fn from(command: LayoutCommand) -> Self {
        use LayoutCommand as L;
        match command {
            L::NextWindow => FocusCommand::Cycle(Cycle::Forward).into(),
            L::PrevWindow => FocusCommand::Cycle(Cycle::Backward).into(),
            L::MoveFocus(direction) => FocusCommand::Move(direction).into(),
            L::MoveFocusToEnd(end) => FocusCommand::ToEnd(end).into(),
            L::SwapWindows(a, b) => {
                let id = |w: rini_ipc::protocol::WindowId| WindowId::new(w.pid, w.idx);
                ArrangeCommand::Swap(id(a), id(b)).into()
            }
            L::MoveNode(direction) => ArrangeCommand::MoveNode(direction).into(),
            L::MoveNodeToEnd(end) => ArrangeCommand::MoveNodeToEnd(end).into(),
            L::ToggleFold(side) => ArrangeCommand::ToggleFold(side).into(),
            L::ExplodeColumn => ArrangeCommand::ExplodeColumn.into(),
            L::ResizeWindowBy { amount, orientation } => {
                ResizeCommand::By { amount, orientation }.into()
            }
            L::CyclePresetColumnWidth => ResizeCommand::CyclePresetWidth.into(),
            L::SnapStrip => StripCommand::Snap.into(),
            L::CenterSelection => StripCommand::CenterSelection.into(),
            L::ToggleWindowFloating => FloatingCommand::ToggleWindowFloating.into(),
            L::ToggleFocusFloating => FloatingCommand::ToggleFocusFloating.into(),
            L::ToggleFullscreenWithinGaps => FloatingCommand::Maximize.into(),
            L::NextWorkspace(skip_empty) => WorkspaceCommand::Next(skip_empty).into(),
            L::PrevWorkspace(skip_empty) => WorkspaceCommand::Prev(skip_empty).into(),
            L::SwitchToWorkspace(index) => WorkspaceCommand::SwitchTo(index).into(),
            L::SwitchToLastWorkspace => WorkspaceCommand::SwitchToLast.into(),
            L::MoveWindowToWorkspace { workspace, follow, window_id } => {
                WorkspaceCommand::MoveWindow { workspace, follow, window_id }.into()
            }
        }
    }
}

macro_rules! family {
    ($($family:ident => $variant:ident),* $(,)?) => {$(
        impl From<$family> for EngineCommand {
            fn from(command: $family) -> Self { Self::$variant(command) }
        }
    )*};
}

family! {
    FocusCommand => Focus,
    ArrangeCommand => Arrange,
    ResizeCommand => Resize,
    StripCommand => Strip,
    FloatingCommand => Floating,
    WorkspaceCommand => Workspace,
}

#[cfg(test)]
mod tests {
    use rini_ipc::protocol::RelativeWorkspace;

    use super::*;

    fn moved(follow: bool) -> LayoutCommand {
        LayoutCommand::MoveWindowToWorkspace {
            workspace: WorkspaceSelector::Relative(RelativeWorkspace::Next),
            follow,
            window_id: Some(4),
        }
    }

    /// Every wire command, and the family and command it becomes.
    #[test]
    fn each_wire_command_lands_in_one_family() {
        let ipc = |pid, idx| rini_ipc::protocol::WindowId { pid, idx };
        let horizontal = ResizeOrientation::Horizontal;
        let table: Vec<(LayoutCommand, EngineCommand)> = vec![
            (
                LayoutCommand::NextWindow,
                FocusCommand::Cycle(Cycle::Forward).into(),
            ),
            (
                LayoutCommand::PrevWindow,
                FocusCommand::Cycle(Cycle::Backward).into(),
            ),
            (
                LayoutCommand::MoveFocus(Direction::Up),
                FocusCommand::Move(Direction::Up).into(),
            ),
            (
                LayoutCommand::MoveFocusToEnd(StripEnd::First),
                FocusCommand::ToEnd(StripEnd::First).into(),
            ),
            (
                LayoutCommand::SwapWindows(ipc(1, 2), ipc(3, 4)),
                ArrangeCommand::Swap(WindowId::new(1, 2), WindowId::new(3, 4)).into(),
            ),
            (
                LayoutCommand::MoveNode(Direction::Left),
                ArrangeCommand::MoveNode(Direction::Left).into(),
            ),
            (
                LayoutCommand::MoveNodeToEnd(StripEnd::Last),
                ArrangeCommand::MoveNodeToEnd(StripEnd::Last).into(),
            ),
            (
                LayoutCommand::ToggleFold(Direction::Right),
                ArrangeCommand::ToggleFold(Direction::Right).into(),
            ),
            (
                LayoutCommand::ExplodeColumn,
                ArrangeCommand::ExplodeColumn.into(),
            ),
            (
                LayoutCommand::ResizeWindowBy {
                    amount: 0.1,
                    orientation: horizontal,
                },
                ResizeCommand::By {
                    amount: 0.1,
                    orientation: horizontal,
                }
                .into(),
            ),
            (
                LayoutCommand::CyclePresetColumnWidth,
                ResizeCommand::CyclePresetWidth.into(),
            ),
            (LayoutCommand::SnapStrip, StripCommand::Snap.into()),
            (
                LayoutCommand::CenterSelection,
                StripCommand::CenterSelection.into(),
            ),
            (
                LayoutCommand::ToggleWindowFloating,
                FloatingCommand::ToggleWindowFloating.into(),
            ),
            (
                LayoutCommand::ToggleFocusFloating,
                FloatingCommand::ToggleFocusFloating.into(),
            ),
            (
                LayoutCommand::ToggleFullscreenWithinGaps,
                FloatingCommand::Maximize.into(),
            ),
            (
                LayoutCommand::NextWorkspace(Some(true)),
                WorkspaceCommand::Next(Some(true)).into(),
            ),
            (
                LayoutCommand::PrevWorkspace(None),
                WorkspaceCommand::Prev(None).into(),
            ),
            (
                LayoutCommand::SwitchToWorkspace(3),
                WorkspaceCommand::SwitchTo(3).into(),
            ),
            (
                LayoutCommand::SwitchToLastWorkspace,
                WorkspaceCommand::SwitchToLast.into(),
            ),
            (
                moved(true),
                WorkspaceCommand::MoveWindow {
                    workspace: WorkspaceSelector::Relative(RelativeWorkspace::Next),
                    follow: true,
                    window_id: Some(4),
                }
                .into(),
            ),
        ];
        for (wire, family) in table {
            assert_eq!(EngineCommand::from(wire.clone()), family, "{wire:?}");
        }
    }

    /// Every switch shows another workspace; a move does only when focus follows the window.
    #[test]
    fn a_switch_shows_another_workspace_and_a_move_only_when_focus_follows() {
        let table = [
            (LayoutCommand::NextWorkspace(None), true),
            (LayoutCommand::PrevWorkspace(Some(true)), true),
            (LayoutCommand::SwitchToWorkspace(2), true),
            (LayoutCommand::SwitchToLastWorkspace, true),
            (moved(true), true),
            (moved(false), false),
        ];
        for (wire, switches) in table {
            let command = EngineCommand::from(wire.clone());
            let workspace = command.workspace().expect("a workspace command");
            assert_eq!(workspace.switches_workspace(), switches, "{wire:?}");
        }
    }

    #[test]
    fn only_a_workspace_command_is_one() {
        assert_eq!(EngineCommand::from(LayoutCommand::NextWindow).workspace(), None);
        assert_eq!(
            EngineCommand::from(LayoutCommand::ToggleWindowFloating).workspace(),
            None
        );
    }
}
