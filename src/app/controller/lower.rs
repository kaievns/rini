//! What a bound action means, as a translation rather than a side effect.
//!
//! Most actions are another name for a `reactor::Command`; a few are things only the application can
//! do: reload the config, run a program. The one that is not a pure rename is a workspace by NAME,
//! resolved against the configured names, which are passed in rather than read so the whole thing
//! stays a function. The config writes workspaces and displays from 1; the wire counts from 0.

use rini_ipc::protocol::{DisplaySelector, RelativeDisplay, RelativeWorkspace, WorkspaceSelector};

use crate::app::reactor;
use crate::input::domain::binding::{
    Action, MonitorReference, SwitcherFilter, SwitcherScope, WorkspaceReference, switch_scope,
};
use crate::layout::{Direction, ResizeOrientation, StripEnd};
use crate::workspaces::LayoutCommand;
use crate::workspaces::domain::selector::same_workspace_name;

/// What the controller should do about a binding.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Lowered {
    /// Send this to the reactor.
    Command(reactor::Command),
    /// Run this argv.
    Spawn(Vec<String>),
    /// Re-read the config file.
    ReloadConfig,
    /// The binding named a workspace that does not exist. Nothing happens, and the caller says so.
    UnknownWorkspace(String),
}

/// Lower a bound action to what carries it out.
///
/// `workspace_names` is the configured list, consulted only to turn a name into an index.
pub(crate) fn lower(action: Action, workspace_names: &[String]) -> Lowered {
    use reactor::Command::{Layout, Reactor};
    use reactor::ReactorCommand as R;

    let layout = |command| Lowered::Command(Layout(command));
    let reactor = |command| Lowered::Command(Reactor(command));
    let resize =
        |amount: f64, orientation| layout(LayoutCommand::ResizeWindowBy { amount, orientation });
    let move_to = |workspace, follow| {
        layout(LayoutCommand::MoveWindowToWorkspace {
            workspace,
            follow,
            window_id: None,
        })
    };
    match action {
        Action::FocusColumnLeft => layout(LayoutCommand::MoveFocus(Direction::Left)),
        Action::FocusColumnRight => layout(LayoutCommand::MoveFocus(Direction::Right)),
        Action::FocusWindowOrMonitorDown => layout(LayoutCommand::MoveFocus(Direction::Down)),
        Action::FocusWindowOrMonitorUp => layout(LayoutCommand::MoveFocus(Direction::Up)),
        Action::MoveWindowLeft => layout(LayoutCommand::MoveNode(Direction::Left)),
        Action::MoveWindowRight => layout(LayoutCommand::MoveNode(Direction::Right)),
        Action::MoveWindowToFirst => layout(LayoutCommand::MoveNodeToEnd(StripEnd::First)),
        Action::MoveWindowToLast => layout(LayoutCommand::MoveNodeToEnd(StripEnd::Last)),
        Action::MoveWindowDownOrToMonitorDown => layout(LayoutCommand::MoveNode(Direction::Down)),
        Action::MoveWindowUpOrToMonitorUp => layout(LayoutCommand::MoveNode(Direction::Up)),
        Action::ToggleFoldLeft => layout(LayoutCommand::ToggleFold(Direction::Left)),
        Action::ToggleFoldRight => layout(LayoutCommand::ToggleFold(Direction::Right)),
        Action::ExplodeColumn => layout(LayoutCommand::ExplodeColumn),
        Action::CycleVisibleWindows => layout(LayoutCommand::NextWindow),
        Action::CycleVisibleWindowsBack => layout(LayoutCommand::PrevWindow),
        Action::ToggleWindowFloating => layout(LayoutCommand::ToggleWindowFloating),
        Action::SwitchFocusBetweenFloatingAndTiling => layout(LayoutCommand::ToggleFocusFloating),
        Action::MaximizeColumn => layout(LayoutCommand::ToggleFullscreenWithinGaps),
        Action::SwitchPresetColumnWidth => layout(LayoutCommand::CyclePresetColumnWidth),
        Action::CenterColumn => layout(LayoutCommand::CenterSelection),
        Action::SetColumnWidth(change) => resize(change.0, ResizeOrientation::Horizontal),
        Action::SetWindowHeight(change) => resize(change.0, ResizeOrientation::Vertical),
        Action::SetColumnWidthOrWindowHeight(change) => resize(change.0, ResizeOrientation::Smart),
        Action::SnapStrip => layout(LayoutCommand::SnapStrip),
        Action::FocusWorkspace(workspace) => match workspace_index(&workspace, workspace_names) {
            Ok(index) => layout(LayoutCommand::SwitchToWorkspace(index)),
            Err(name) => Lowered::UnknownWorkspace(name),
        },
        Action::FocusWorkspaceDown(skip_empty) => layout(LayoutCommand::NextWorkspace(skip_empty)),
        Action::FocusWorkspaceUp(skip_empty) => layout(LayoutCommand::PrevWorkspace(skip_empty)),
        Action::FocusWorkspacePrevious => layout(LayoutCommand::SwitchToLastWorkspace),
        Action::MoveWindowToWorkspace(workspace, follow) => {
            match workspace_index(&workspace, workspace_names) {
                Ok(index) => move_to(WorkspaceSelector::Index(index), follow),
                Err(name) => Lowered::UnknownWorkspace(name),
            }
        }
        Action::MoveWindowToWorkspaceDown(follow) => {
            move_to(WorkspaceSelector::Relative(RelativeWorkspace::Next), follow)
        }
        Action::MoveWindowToWorkspaceUp(follow) => {
            move_to(WorkspaceSelector::Relative(RelativeWorkspace::Prev), follow)
        }
        Action::FocusMonitorLeft => reactor(R::FocusDisplay(direction(Direction::Left))),
        Action::FocusMonitorRight => reactor(R::FocusDisplay(direction(Direction::Right))),
        Action::FocusMonitorUp => reactor(R::FocusDisplay(direction(Direction::Up))),
        Action::FocusMonitorDown => reactor(R::FocusDisplay(direction(Direction::Down))),
        Action::FocusMonitorNext => reactor(R::FocusDisplay(next())),
        Action::FocusMonitor(monitor) => reactor(R::FocusDisplay(monitor_selector(monitor))),
        Action::MoveWindowToMonitorLeft => move_to_display(direction(Direction::Left)),
        Action::MoveWindowToMonitorRight => move_to_display(direction(Direction::Right)),
        Action::MoveWindowToMonitorUp => move_to_display(direction(Direction::Up)),
        Action::MoveWindowToMonitorDown => move_to_display(direction(Direction::Down)),
        Action::MoveWindowToMonitorNext => move_to_display(next()),
        Action::MoveWindowToMonitor(monitor) => move_to_display(monitor_selector(monitor)),
        Action::MoveMouseToMonitorLeft => {
            reactor(R::MoveMouseToDisplay(direction(Direction::Left)))
        }
        Action::MoveMouseToMonitorRight => {
            reactor(R::MoveMouseToDisplay(direction(Direction::Right)))
        }
        Action::MoveMouseToMonitorUp => reactor(R::MoveMouseToDisplay(direction(Direction::Up))),
        Action::MoveMouseToMonitorDown => {
            reactor(R::MoveMouseToDisplay(direction(Direction::Down)))
        }
        Action::MoveMouseToMonitorNext => reactor(R::MoveMouseToDisplay(next())),
        Action::MoveMouseToMonitor(monitor) => {
            reactor(R::MoveMouseToDisplay(monitor_selector(monitor)))
        }
        Action::ToggleSpaceActivated => reactor(R::ToggleSpaceActivated),
        // No window server id: a hotkey means the focused window, and the reactor resolves that.
        Action::CloseWindow => reactor(R::CloseWindow { window_server_id: None }),
        Action::Spawn(argv) => Lowered::Spawn(argv),
        Action::SpawnSh(command) => Lowered::Spawn(crate::app::startup::shell(&command)),
        Action::LoadConfigFile => Lowered::ReloadConfig,
        Action::Quit => reactor(R::SaveAndExit),
        Action::RedistributeWindows => reactor(R::RedistributeWindows),
        Action::NextWindow { scope, filter } => switch(false, scope, filter),
        Action::PreviousWindow { scope, filter } => switch(true, scope, filter),
    }
}

fn direction(direction: Direction) -> DisplaySelector { DisplaySelector::Direction(direction) }

fn next() -> DisplaySelector { DisplaySelector::Relative(RelativeDisplay::Next) }

fn move_to_display(selector: DisplaySelector) -> Lowered {
    Lowered::Command(reactor::Command::Reactor(
        reactor::ReactorCommand::MoveWindowToDisplay { selector, window_id: None },
    ))
}

fn monitor_selector(monitor: MonitorReference) -> DisplaySelector {
    match monitor {
        MonitorReference::Index(index) => DisplaySelector::Index(index.saturating_sub(1)),
        MonitorReference::Uuid(uuid) => DisplaySelector::Uuid(uuid),
    }
}

fn switch(backward: bool, scope: Option<SwitcherScope>, filter: Option<SwitcherFilter>) -> Lowered {
    Lowered::Command(reactor::Command::Reactor(
        reactor::ReactorCommand::SwitchWindow {
            backward,
            scope: switch_scope(scope, filter),
        },
    ))
}

/// An index counts from 1 and is taken at face value; a name is looked up in the configured order.
///
/// An index is NOT range-checked here. The configured names and the workspaces that exist are two
/// different lists — a workspace can be created at runtime — so the reactor is the only thing that
/// knows whether an index is reachable.
fn workspace_index(workspace: &WorkspaceReference, names: &[String]) -> Result<usize, String> {
    match workspace {
        WorkspaceReference::Index(index) => Ok(index.saturating_sub(1)),
        WorkspaceReference::Name(name) => names
            .iter()
            .position(|configured| same_workspace_name(configured, name))
            .ok_or_else(|| name.clone()),
    }
}

#[cfg(test)]
mod tests;
