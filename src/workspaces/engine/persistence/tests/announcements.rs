//! What the engine tells the application it did, which subscribers see over IPC.

use crate::workspaces::domain::display_memory::DisplayMemory;
use objc2_core_foundation::{CGPoint, CGRect, CGSize};

use super::*;
use crate::windows::domain::info::WindowInfo;
use crate::windows::domain::state::WindowState;
use crate::workspaces::LayoutEvent;
use rini_core::ids::WindowServerId;

/// The maximize toggle has to report `changed`, because that is the only thing that routes it into
/// `update_layout` and so into `AnimationManager::animate_layout`. A response of `default()` here
/// would leave the windows to jump to their new columns with no flight.
#[test]
fn maximizing_reports_a_geometry_change_so_the_move_is_animated() {
    use crate::workspaces::engine::LayoutCommand;

    const DISPLAY: &str = "37D8832A-2D66-02CA-B9F7-8F30A301B230";
    let space = SpaceId::new(11);
    let mut engine = test_engine();
    let mut memory = DisplayMemory::default();
    let mut store = WindowStore::default();
    let _ = engine.handle_event(
        &mut store,
        &mut memory,
        LayoutEvent::SpaceExposed(space, CGSize::new(1728.0, 1085.0)),
    );
    engine.update_space_display(&mut memory, space, Some(DISPLAY.to_owned()));
    engine.set_connected_displays(vec![DISPLAY.to_owned()]);

    let stacked = [WindowId::new(500, 1), WindowId::new(500, 2)];
    for window in stacked {
        let frame = CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(800.0, 600.0));
        store.insert_window(
            window,
            WindowState {
                info: WindowInfo {
                    is_standard: true,
                    is_root: true,
                    is_minimized: false,
                    is_resizable: true,
                    min_size: None,
                    max_size: None,
                    title: "term".into(),
                    frame,
                    sys_id: Some(WindowServerId::new(window.idx.get())),
                    bundle_id: Some("com.mitchellh.ghostty".into()),
                    path: None,
                    ax_role: None,
                    ax_subrole: None,
                    is_modal: false,
                },
                frame_monotonic: frame,
                is_manageable: true,
                ignore_app_rule: false,
            },
        );
        let _ =
            engine.handle_event(&mut store, &mut memory, LayoutEvent::WindowAdded(space, window));
    }
    engine.focused_window = Some(stacked[1]);
    let _ = engine.handle_command(
        &mut store,
        &mut memory,
        Some(space),
        &[space],
        &HashMap::default(),
        LayoutCommand::ToggleFold(crate::layout::Direction::Left),
    );

    let response = engine.handle_command(
        &mut store,
        &mut memory,
        Some(space),
        &[space],
        &HashMap::default(),
        LayoutCommand::ToggleFullscreenWithinGaps,
    );
    assert!(
        response.changed,
        "without this the reactor runs no layout pass and nothing animates"
    );
    assert_eq!(response.raise_windows, vec![stacked[1]]);
}

/// What a command announces is now answerable without a channel: the engine fills an outbox and the
/// application drains it. Before this the sender lived on the engine, so asserting a broadcast meant
/// standing up IPC, and nothing did.
#[test]
fn switching_workspace_announces_the_switch_and_its_windows() {
    use crate::workspaces::broadcast::BroadcastEvent;

    let space = SpaceId::new(31);
    let mut engine = test_engine();
    let mut memory = DisplayMemory::default();
    let mut store = WindowStore::default();
    let _ = engine.handle_event(
        &mut store,
        &mut memory,
        LayoutEvent::SpaceExposed(space, CGSize::new(1728.0, 1085.0)),
    );
    // Whatever exposing a space announced is not what this test is about.
    let _ = engine.drain_broadcasts();

    let workspaces: Vec<_> = engine
        .virtual_workspace_manager
        .list_workspaces(space)
        .into_iter()
        .map(|(id, _)| id)
        .collect();
    assert!(workspaces.len() > 1, "the fixture needs somewhere to switch to");
    let _ = engine.switch_to_workspace(&store, space, 1, None);

    let announced = engine.drain_broadcasts();
    assert!(
        announced
            .iter()
            .any(|event| matches!(event, BroadcastEvent::WorkspaceChanged { .. })),
        "a subscriber has to hear that the workspace changed: {announced:?}"
    );
    assert!(
        engine.drain_broadcasts().is_empty(),
        "draining takes them, so the application cannot send the same event twice"
    );
}
