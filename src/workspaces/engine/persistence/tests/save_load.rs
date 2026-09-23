//! A round trip: what a save writes, and what a load makes of it.

use crate::workspaces::domain::display_memory::DisplayMemory;
use objc2_core_foundation::CGSize;

use super::*;
use crate::windows::domain::info::WindowInfo;
use crate::windows::domain::state::WindowState;
use crate::workspaces::LayoutEvent;
use rini_core::ids::WindowServerId;

#[test]
fn save_and_load_arms_fingerprint_reconciliation() {
    let mut engine = test_engine();
    let mut memory = DisplayMemory::default();
    let window = WindowId::new(42, 7);
    let mut window_store = WindowStore::default();
    let space = SpaceId::new(123);
    let _ = engine.handle_event(
        &mut window_store,
        &mut memory,
        LayoutEvent::SpaceExposed(space, CGSize::new(1200.0, 800.0)),
    );
    let _ = engine.handle_event(
        &mut window_store,
        &mut memory,
        LayoutEvent::WindowAdded(space, window),
    );
    engine.persistence.windows.insert(
        window,
        WindowFingerprint {
            window_server_id: Some(9001),
            title: Some("Project".into()),
            width: 900.0,
            height: 700.0,
            app_id: Some("com.example.editor".into()),
        },
    );
    let path = std::env::temp_dir().join(format!(
        "rini-layout-restore-test-{}-{}.ron",
        std::process::id(),
        window.idx.get()
    ));

    engine.save(&mut memory, path.clone()).unwrap();
    let loaded = LayoutEngine::load(path.clone()).unwrap();
    let _ = std::fs::remove_file(path);

    assert_eq!(loaded.persistence.windows[&window].window_server_id, Some(9001));
    assert!(loaded.persistence.pending_windows.contains(&window));
    assert!(loaded.restored_location_for_window(window).is_some());
}

#[test]
fn full_save_records_floating_window_in_its_inactive_workspace() {
    let mut engine = test_engine();
    let mut memory = DisplayMemory::default();
    let mut window_store = WindowStore::default();
    let space = SpaceId::new(122);
    let size = CGSize::new(1200.0, 800.0);
    let frame = objc2_core_foundation::CGRect::new(
        objc2_core_foundation::CGPoint::new(40.0, 50.0),
        CGSize::new(640.0, 480.0),
    );
    let window = WindowId::new(41, 6);
    let _ = engine.handle_event(
        &mut window_store,
        &mut memory,
        LayoutEvent::SpaceExposed(space, size),
    );
    let active_workspace = engine.active_workspace(space).unwrap();
    let inactive_workspace = engine
        .virtual_workspace_manager
        .list_workspaces(space)
        .into_iter()
        .map(|(workspace, _)| workspace)
        .find(|workspace| *workspace != active_workspace)
        .unwrap();
    window_store.insert_window(
        window,
        WindowState {
            info: WindowInfo {
                is_standard: true,
                is_root: true,
                is_minimized: false,
                is_resizable: true,
                min_size: None,
                max_size: None,
                title: "Inactive floating".into(),
                frame,
                sys_id: Some(WindowServerId::new(4106)),
                bundle_id: Some("com.example.floating".into()),
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
    assert!(engine.virtual_workspace_manager.assign_window_to_workspace(
        &mut window_store,
        space,
        window,
        inactive_workspace,
    ));
    engine.floating.add_floating(window);
    let path = std::env::temp_dir().join(format!(
        "rini-inactive-floating-save-test-{}-{}.ron",
        std::process::id(),
        space.get(),
    ));

    engine
        .save_current_layout(path.clone(), &window_store, &mut memory, Some(space))
        .unwrap();
    let loaded = LayoutEngine::load(path.clone()).unwrap();
    let _ = std::fs::remove_file(path);

    assert_eq!(
        loaded.restored_location_for_window(window),
        Some((space, inactive_workspace)),
    );
    assert_eq!(
        loaded.floating_positions.get(space, inactive_workspace, window),
        Some(frame),
    );
    assert!(loaded.floating.is_floating(window));
}

#[test]
fn full_save_removes_stale_floating_frame_from_a_tiled_window() {
    let mut engine = test_engine();
    let mut memory = DisplayMemory::default();
    let mut window_store = WindowStore::default();
    let space = SpaceId::new(124);
    let size = CGSize::new(1200.0, 800.0);
    let frame = objc2_core_foundation::CGRect::new(
        objc2_core_foundation::CGPoint::new(80.0, 90.0),
        CGSize::new(700.0, 500.0),
    );
    let window = WindowId::new(41, 7);
    let _ = engine.handle_event(
        &mut window_store,
        &mut memory,
        LayoutEvent::SpaceExposed(space, size),
    );
    let workspace = engine.active_workspace(space).unwrap();
    window_store.insert_window(
        window,
        WindowState {
            info: WindowInfo {
                is_standard: true,
                is_root: true,
                is_minimized: false,
                is_resizable: true,
                min_size: None,
                max_size: None,
                title: "Tiled".into(),
                frame,
                sys_id: Some(WindowServerId::new(4107)),
                bundle_id: Some("com.example.tiled".into()),
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
    assert!(engine.virtual_workspace_manager.assign_window_to_workspace(
        &mut window_store,
        space,
        window,
        workspace,
    ));
    engine.add_window_to_layout(&mut window_store, &mut memory, space, window);
    // Model stale state left behind by an earlier floating-to-tiled transition.
    engine.floating_positions.store(space, workspace, window, frame);
    let path = std::env::temp_dir().join(format!(
        "rini-stale-floating-save-test-{}-{}.ron",
        std::process::id(),
        space.get(),
    ));

    engine
        .save_current_layout(path.clone(), &window_store, &mut memory, Some(space))
        .unwrap();
    let loaded = LayoutEngine::load(path.clone()).unwrap();
    let _ = std::fs::remove_file(path);

    assert_eq!(
        loaded.restored_location_for_window(window),
        Some((space, workspace))
    );
    assert_eq!(loaded.floating_positions.get(space, workspace, window), None);
    assert!(!loaded.floating.is_floating(window));
}

#[test]
fn load_does_not_arm_locationless_fingerprints() {
    let mut engine = test_engine();
    let mut memory = DisplayMemory::default();
    let orphan = WindowId::new(42, 8);
    let space = SpaceId::new(122);
    let mut window_store = WindowStore::default();
    let _ = engine.handle_event(
        &mut window_store,
        &mut memory,
        LayoutEvent::SpaceExposed(space, CGSize::new(1200.0, 800.0)),
    );
    let workspace = engine.active_workspace(space).unwrap();
    engine.persistence.windows.insert(
        orphan,
        WindowFingerprint {
            window_server_id: None,
            title: Some("Untitled".into()),
            width: 800.0,
            height: 600.0,
            app_id: Some("com.example.orphan".into()),
        },
    );
    engine
        .virtual_workspace_manager
        .set_last_focused_window(space, workspace, Some(orphan));

    let loaded = LayoutEngine::deserialize_from_str(&engine.serialize_to_string(&memory)).unwrap();

    assert!(loaded.persistence.windows.contains_key(&orphan));
    assert!(!loaded.persistence.pending_windows.contains(&orphan));
    assert_eq!(
        loaded.virtual_workspace_manager.last_focused_window(space, workspace),
        None,
    );
}

#[test]
fn load_removes_serialized_window_state_without_a_fingerprint() {
    let mut engine = test_engine();
    let mut memory = DisplayMemory::default();
    let mut window_store = WindowStore::default();
    let space = SpaceId::new(123);
    let ghost = WindowId::new(42, 9);
    let _ = engine.handle_event(
        &mut window_store,
        &mut memory,
        LayoutEvent::SpaceExposed(space, CGSize::new(1200.0, 800.0)),
    );
    let _ = engine.handle_event(
        &mut window_store,
        &mut memory,
        LayoutEvent::WindowAdded(space, ghost),
    );
    let workspace = engine.active_workspace(space).unwrap();
    engine.floating.add_floating(ghost);
    engine.floating_positions.store(
        space,
        workspace,
        ghost,
        objc2_core_foundation::CGRect::new(
            objc2_core_foundation::CGPoint::new(10.0, 20.0),
            CGSize::new(700.0, 500.0),
        ),
    );
    engine
        .virtual_workspace_manager
        .set_last_focused_window(space, workspace, Some(ghost));
    assert!(!engine.persistence.windows.contains_key(&ghost));

    let loaded = LayoutEngine::deserialize_from_str(&engine.serialize_to_string(&memory)).unwrap();
    let layout = loaded.workspace_layouts.active(space, workspace).unwrap();

    assert!(!loaded.workspace_tree(workspace).contains_window(layout, ghost));
    assert!(!loaded.floating.is_floating(ghost));
    assert_eq!(loaded.floating_positions.get(space, workspace, ghost), None);
    assert_eq!(
        loaded.virtual_workspace_manager.last_focused_window(space, workspace),
        None
    );
}

#[test]
fn load_heals_disagreeing_tiled_and_floating_ownership() {
    let mut engine = test_engine();
    let mut memory = DisplayMemory::default();
    let mut window_store = WindowStore::default();
    let space = SpaceId::new(125);
    let size = CGSize::new(1200.0, 800.0);
    let marked_without_frame = WindowId::new(42, 10);
    let agreed_floating = WindowId::new(42, 11);
    let frame_without_marker = WindowId::new(42, 12);
    let _ = engine.handle_event(
        &mut window_store,
        &mut memory,
        LayoutEvent::SpaceExposed(space, size),
    );
    for window in [marked_without_frame, agreed_floating, frame_without_marker] {
        let _ = engine.handle_event(
            &mut window_store,
            &mut memory,
            LayoutEvent::WindowAdded(space, window),
        );
        engine.persistence.windows.insert(
            window,
            WindowFingerprint {
                window_server_id: Some(window.idx.get()),
                title: Some(format!("window-{}", window.idx)),
                width: 600.0,
                height: 500.0,
                app_id: Some("com.example.editor".into()),
            },
        );
    }
    let workspaces = engine.virtual_workspace_manager.existing_workspaces(space);
    let active = engine.active_workspace(space).unwrap();
    let other = workspaces.iter().find(|(workspace, _)| *workspace != active).unwrap().0;
    let frame = objc2_core_foundation::CGRect::new(
        objc2_core_foundation::CGPoint::new(10.0, 20.0),
        CGSize::new(600.0, 500.0),
    );
    engine.floating.add_floating(marked_without_frame);
    engine.floating.add_floating(agreed_floating);
    engine.floating_positions.store(space, active, agreed_floating, frame);
    engine.floating_positions.store(space, other, agreed_floating, frame);
    engine.floating_positions.store(space, active, frame_without_marker, frame);
    engine.floating.set_last_focus(Some(frame_without_marker));

    let loaded = LayoutEngine::deserialize_from_str(&engine.serialize_to_string(&memory)).unwrap();

    assert!(!loaded.floating.is_floating(marked_without_frame));
    assert!(loaded.restored_location_for_window(marked_without_frame).is_some());
    assert!(loaded.floating.is_floating(agreed_floating));
    assert_eq!(
        loaded.floating_positions.locations_for_window(agreed_floating).len(),
        1
    );
    assert!(
        loaded
            .workspace_layouts
            .all_layouts()
            .into_iter()
            .all(|(_, workspace, layout)| {
                !loaded.workspace_tree(workspace).contains_window(layout, agreed_floating)
            })
    );
    assert!(!loaded.floating.is_floating(frame_without_marker));
    assert!(loaded.floating_positions.locations_for_window(frame_without_marker).is_empty());
    assert!(loaded.restored_location_for_window(frame_without_marker).is_some());
    assert_ne!(loaded.floating.last_focus(), Some(frame_without_marker));
}

#[test]
fn app_close_removes_saved_fingerprints() {
    let mut engine = test_engine();
    let mut memory = DisplayMemory::default();
    let mut window_store = WindowStore::default();
    let window = WindowId::new(42, 7);
    engine.persistence.windows.insert(
        window,
        WindowFingerprint {
            window_server_id: Some(9),
            title: Some("Closed".into()),
            width: 400.0,
            height: 300.0,
            app_id: Some("com.example.closed".into()),
        },
    );
    engine.persistence.pending_windows.insert(window);

    let _ = engine.handle_event(
        &mut window_store,
        &mut memory,
        LayoutEvent::AppClosed(window.pid),
    );

    assert!(!engine.persistence.windows.contains_key(&window));
    assert!(!engine.persistence.pending_windows.contains(&window));
}
