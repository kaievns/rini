use rini_ipc::protocol::{
    MetricsCommand, RestoreScope, RestoreSource, SwitchScope, WorkspaceSelector,
};
use strum::VariantNames;

use super::*;
use crate::app::reactor::{Command, ReactorCommand as R};
use crate::input::domain::binding::{Decimal, SizeChange};

fn names() -> Vec<String> {
    ["Main", "Development", "Communication", "next"].map(String::from).to_vec()
}

fn layout(command: LayoutCommand) -> Lowered { Lowered::Command(Command::Layout(command)) }

fn reactor(command: R) -> Lowered { Lowered::Command(Command::Reactor(command)) }

fn moved(workspace: WorkspaceSelector, follow: bool) -> Lowered {
    layout(LayoutCommand::MoveWindowToWorkspace {
        workspace,
        follow,
        window_id: None,
    })
}

fn to_display(selector: DisplaySelector) -> Lowered {
    reactor(R::MoveWindowToDisplay { selector, window_id: None })
}

fn switch(backward: bool, scope: SwitchScope) -> Lowered {
    reactor(R::SwitchWindow { backward, scope })
}

/// The design's table, one row per action: what each one is on the wire.
fn table() -> Vec<(Action, Lowered)> {
    use Direction::*;
    let d = DisplaySelector::Direction;
    let next = DisplaySelector::Relative(RelativeDisplay::Next);
    let resize =
        |amount, orientation| layout(LayoutCommand::ResizeWindowBy { amount, orientation });
    vec![
        (Action::FocusColumnLeft, layout(LayoutCommand::MoveFocus(Left))),
        (Action::FocusColumnRight, layout(LayoutCommand::MoveFocus(Right))),
        (
            Action::FocusWindowOrMonitorDown,
            layout(LayoutCommand::MoveFocus(Down)),
        ),
        (
            Action::FocusWindowOrMonitorUp,
            layout(LayoutCommand::MoveFocus(Up)),
        ),
        (Action::MoveWindowLeft, layout(LayoutCommand::MoveNode(Left))),
        (Action::MoveWindowRight, layout(LayoutCommand::MoveNode(Right))),
        (
            Action::MoveWindowDownOrToMonitorDown,
            layout(LayoutCommand::MoveNode(Down)),
        ),
        (
            Action::MoveWindowUpOrToMonitorUp,
            layout(LayoutCommand::MoveNode(Up)),
        ),
        (Action::ToggleFoldLeft, layout(LayoutCommand::ToggleFold(Left))),
        (Action::ToggleFoldRight, layout(LayoutCommand::ToggleFold(Right))),
        (Action::ToggleStack, layout(LayoutCommand::ToggleStack)),
        (Action::CycleVisibleWindows, layout(LayoutCommand::NextWindow)),
        (
            Action::CycleVisibleWindowsBack,
            layout(LayoutCommand::PrevWindow),
        ),
        (
            Action::ToggleWindowFloating,
            layout(LayoutCommand::ToggleWindowFloating),
        ),
        (
            Action::SwitchFocusBetweenFloatingAndTiling,
            layout(LayoutCommand::ToggleFocusFloating),
        ),
        (
            Action::MaximizeColumn,
            layout(LayoutCommand::ToggleFullscreenWithinGaps),
        ),
        (
            Action::SwitchPresetColumnWidth,
            layout(LayoutCommand::CyclePresetColumnWidth),
        ),
        (Action::CenterColumn, layout(LayoutCommand::CenterSelection)),
        (
            Action::SetColumnWidth(SizeChange(0.05)),
            resize(0.05, ResizeOrientation::Horizontal),
        ),
        (
            Action::SetWindowHeight(SizeChange(-0.05)),
            resize(-0.05, ResizeOrientation::Vertical),
        ),
        (
            Action::SetColumnWidthOrWindowHeight(SizeChange(0.1)),
            resize(0.1, ResizeOrientation::Smart),
        ),
        (
            Action::ScrollStrip(Decimal(0.5)),
            layout(LayoutCommand::ScrollStrip { delta: 0.5 }),
        ),
        (Action::SnapStrip, layout(LayoutCommand::SnapStrip)),
        (
            Action::FocusWorkspace(WorkspaceReference::Index(1)),
            layout(LayoutCommand::SwitchToWorkspace(0)),
        ),
        (
            Action::FocusWorkspaceDown(Some(true)),
            layout(LayoutCommand::NextWorkspace(Some(true))),
        ),
        (
            Action::FocusWorkspaceUp(None),
            layout(LayoutCommand::PrevWorkspace(None)),
        ),
        (
            Action::FocusWorkspacePrevious,
            layout(LayoutCommand::SwitchToLastWorkspace),
        ),
        (
            Action::MoveWindowToWorkspace(WorkspaceReference::Index(2), true),
            moved(WorkspaceSelector::Index(1), true),
        ),
        (
            Action::MoveWindowToWorkspaceDown(true),
            moved(WorkspaceSelector::Name("next".into()), true),
        ),
        (
            Action::MoveWindowToWorkspaceUp(false),
            moved(WorkspaceSelector::Name("prev".into()), false),
        ),
        (Action::CreateWorkspace, layout(LayoutCommand::CreateWorkspace)),
        (Action::FocusMonitorLeft, reactor(R::FocusDisplay(d(Left)))),
        (Action::FocusMonitorRight, reactor(R::FocusDisplay(d(Right)))),
        (Action::FocusMonitorUp, reactor(R::FocusDisplay(d(Up)))),
        (Action::FocusMonitorDown, reactor(R::FocusDisplay(d(Down)))),
        (Action::FocusMonitorNext, reactor(R::FocusDisplay(next.clone()))),
        (
            Action::FocusMonitor(MonitorReference::Index(2)),
            reactor(R::FocusDisplay(DisplaySelector::Index(1))),
        ),
        (Action::MoveWindowToMonitorLeft, to_display(d(Left))),
        (Action::MoveWindowToMonitorRight, to_display(d(Right))),
        (Action::MoveWindowToMonitorUp, to_display(d(Up))),
        (Action::MoveWindowToMonitorDown, to_display(d(Down))),
        (Action::MoveWindowToMonitorNext, to_display(next.clone())),
        (
            Action::MoveWindowToMonitor(MonitorReference::Uuid("D".into())),
            to_display(DisplaySelector::Uuid("D".into())),
        ),
        (
            Action::MoveMouseToMonitorLeft,
            reactor(R::MoveMouseToDisplay(d(Left))),
        ),
        (
            Action::MoveMouseToMonitorRight,
            reactor(R::MoveMouseToDisplay(d(Right))),
        ),
        (
            Action::MoveMouseToMonitorUp,
            reactor(R::MoveMouseToDisplay(d(Up))),
        ),
        (
            Action::MoveMouseToMonitorDown,
            reactor(R::MoveMouseToDisplay(d(Down))),
        ),
        (
            Action::MoveMouseToMonitorNext,
            reactor(R::MoveMouseToDisplay(next)),
        ),
        (
            Action::MoveMouseToMonitor(MonitorReference::Index(1)),
            reactor(R::MoveMouseToDisplay(DisplaySelector::Index(0))),
        ),
        (Action::SwitchSpaceLeft, reactor(R::SwitchSpace(Left))),
        (Action::SwitchSpaceRight, reactor(R::SwitchSpace(Right))),
        (Action::ToggleSpaceActivated, reactor(R::ToggleSpaceActivated)),
        (
            Action::CloseWindow,
            reactor(R::CloseWindow { window_server_id: None }),
        ),
        (
            Action::Spawn(vec!["open".into(), "-a".into(), "My App".into()]),
            Lowered::Spawn(vec!["open".into(), "-a".into(), "My App".into()]),
        ),
        (
            Action::SpawnSh("echo $HOME".into()),
            Lowered::Spawn(vec!["/bin/sh".into(), "-c".into(), "echo $HOME".into()]),
        ),
        (Action::LoadConfigFile, Lowered::ReloadConfig),
        (Action::Quit, reactor(R::SaveAndExit)),
        (
            Action::SaveLayout("/tmp/l.ron".into()),
            reactor(R::SaveLayout { path: "/tmp/l.ron".into() }),
        ),
        (
            Action::RestoreLayout {
                path: "/tmp/l.ron".into(),
                scope: RestoreScope::Space,
                source: None,
            },
            reactor(R::RestoreLayout {
                path: "/tmp/l.ron".into(),
                scope: RestoreScope::Space,
                source: RestoreSource::SavedActiveSpace,
            }),
        ),
        (Action::RedistributeWindows, reactor(R::RedistributeWindows)),
        (Action::Serialize, reactor(R::Serialize)),
        (Action::Debug, reactor(R::Debug)),
        (Action::DebugWarmSnapshots, reactor(R::DebugWarmSnapshots)),
        (
            Action::DebugOverlaySlide {
                dx: -400,
                dy: 0,
                duration_ms: 250,
            },
            reactor(R::DebugOverlaySlide {
                dx: -400,
                dy: 0,
                duration_ms: 250,
            }),
        ),
        (
            Action::ShowTiming,
            Lowered::Command(Command::Metrics(MetricsCommand::ShowTiming)),
        ),
        (
            Action::NextWindow { scope: None, filter: None },
            switch(false, SwitchScope::Everything),
        ),
        (
            Action::PreviousWindow { scope: None, filter: None },
            switch(true, SwitchScope::Everything),
        ),
    ]
}

#[test]
fn every_action_lowers_to_its_wire_command() {
    let table = table();
    assert_eq!(table.len(), Action::VARIANTS.len(), "a row per action");
    for (action, expected) in table {
        assert_eq!(lower(action.clone(), &names()), expected, "{action:?}");
    }
}

/// Every action lowers to something distinct. Sixty-seven near-identical arms is where a copy-paste
/// sends two bindings to one command and nothing notices.
#[test]
fn no_two_actions_lower_to_the_same_thing() {
    let mut seen: Vec<(Action, Lowered)> = Vec::new();
    for (action, _) in table() {
        let got = lower(action.clone(), &names());
        if let Some((other, _)) = seen.iter().find(|(_, earlier)| *earlier == got) {
            panic!("{action:?} lowers to what {other:?} does: {got:?}");
        }
        seen.push((action, got));
    }
}

#[test]
fn the_switcher_scopes_are_one_command_with_a_scope() {
    use crate::input::domain::binding::{SwitcherFilter, SwitcherScope};
    for (scope, filter, expected) in [
        (Some(SwitcherScope::All), None, SwitchScope::Everything),
        (Some(SwitcherScope::Workspace), None, SwitchScope::Workspace),
        (None, Some(SwitcherFilter::AppId), SwitchScope::App),
    ] {
        assert_eq!(
            lower(Action::NextWindow { scope, filter }, &names()),
            switch(false, expected)
        );
        assert_eq!(
            lower(Action::PreviousWindow { scope, filter }, &names()),
            switch(true, expected)
        );
    }
}

#[test]
fn a_workspace_name_becomes_its_position_in_the_configured_order() {
    assert_eq!(
        lower(
            Action::FocusWorkspace(WorkspaceReference::Name("Communication".into())),
            &names()
        ),
        layout(LayoutCommand::SwitchToWorkspace(2))
    );
}

/// A workspace NAMED `next` is a workspace, not the step the wire spells the same way.
#[test]
fn a_workspace_named_next_is_reachable_by_name() {
    assert_eq!(
        lower(
            Action::MoveWindowToWorkspace(WorkspaceReference::Name("next".into()), true),
            &names()
        ),
        moved(WorkspaceSelector::Index(3), true)
    );
}

/// A name that is not configured does nothing, and says which name.
#[test]
fn an_unconfigured_workspace_name_is_refused_by_name() {
    assert_eq!(
        lower(
            Action::FocusWorkspace(WorkspaceReference::Name("Gaming".into())),
            &names()
        ),
        Lowered::UnknownWorkspace("Gaming".into())
    );
}

/// An index beyond the configured names is still passed through: the names and the workspaces
/// that exist are different lists, and only the reactor knows the second one.
#[test]
fn an_index_is_not_checked_against_the_configured_names() {
    assert_eq!(
        lower(Action::FocusWorkspace(WorkspaceReference::Index(10)), &names()),
        layout(LayoutCommand::SwitchToWorkspace(9))
    );
}

/// `set-column-width "+5%"` is what `resize_window_grow` was: the same step, the same orientation.
#[test]
fn a_five_percent_step_is_the_old_grow_and_shrink() {
    assert_eq!(
        lower(Action::SetColumnWidth("+5%".parse().unwrap()), &names()),
        layout(LayoutCommand::ResizeWindowBy {
            amount: 0.05,
            orientation: ResizeOrientation::Horizontal
        })
    );
    assert_eq!(
        lower(Action::SetColumnWidth("-5%".parse().unwrap()), &names()),
        layout(LayoutCommand::ResizeWindowBy {
            amount: -0.05,
            orientation: ResizeOrientation::Horizontal
        })
    );
}
