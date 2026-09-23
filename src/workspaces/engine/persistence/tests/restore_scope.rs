//! How much of a layout a restore replaces, and what it leaves alone.
//!
//! A scoped restore must not consume a live window from outside its target, which is what most of
//! these are about.

use crate::workspaces::domain::display_memory::DisplayMemory;
use objc2_core_foundation::CGSize;

use super::*;
use crate::windows::domain::info::WindowInfo;
use crate::windows::domain::state::WindowState;
use crate::workspaces::LayoutEvent;
use rini_core::ids::WindowServerId;

#[test]
fn workspace_restore_discards_unmatched_scoped_windows_and_floating_state() {
    let mut snapshot = test_engine();
    let mut snapshot_memory = DisplayMemory::default();
    let mut snapshot_store = WindowStore::default();
    let space = SpaceId::new(124);
    let tiled = WindowId::new(10, 1);
    let floating = WindowId::new(10, 2);
    let out_of_scope = WindowId::new(10, 3);
    let size = CGSize::new(1200.0, 800.0);
    let _ = snapshot.handle_event(
        &mut snapshot_store,
        &mut snapshot_memory,
        LayoutEvent::SpaceExposed(space, size),
    );
    let workspaces = snapshot.virtual_workspace_manager.list_workspaces(space);
    let source_workspace = workspaces[0].0;
    let other_workspace = workspaces[1].0;
    let source_layout = snapshot.workspace_layouts.active(space, source_workspace).unwrap();
    let other_layout = snapshot.workspace_layouts.active(space, other_workspace).unwrap();
    snapshot
        .workspace_tree_mut(source_workspace)
        .add_window_after_selection(source_layout, tiled);
    snapshot
        .workspace_tree_mut(other_workspace)
        .add_window_after_selection(other_layout, out_of_scope);
    let floating_frame = objc2_core_foundation::CGRect::new(
        objc2_core_foundation::CGPoint::new(20.0, 30.0),
        CGSize::new(500.0, 400.0),
    );
    snapshot.floating.add_floating(floating);
    snapshot
        .floating_positions
        .store(space, source_workspace, floating, floating_frame);
    for window in [tiled, floating, out_of_scope] {
        snapshot.persistence.windows.insert(
            window,
            WindowFingerprint {
                window_server_id: Some(window.idx.get()),
                title: Some(format!("window-{}", window.idx.get())),
                width: 500.0,
                height: 400.0,
                app_id: Some("com.example.restore".into()),
            },
        );
    }
    let path = std::env::temp_dir().join(format!(
        "rini-scoped-layout-restore-test-{}-{}.ron",
        std::process::id(),
        space.get(),
    ));
    snapshot.save(&mut snapshot_memory, path.clone()).unwrap();

    let mut engine = test_engine();
    let mut engine_memory = DisplayMemory::default();

    let _memory = DisplayMemory::default();
    let mut window_store = WindowStore::default();
    let _ = engine.handle_event(
        &mut window_store,
        &mut engine_memory,
        LayoutEvent::SpaceExposed(space, size),
    );
    let target_workspace = engine.active_workspace(space).unwrap();
    let report = engine
        .restore_layout(
            path.clone(),
            RestoreRequest::new(RestoreScope::Workspace, space),
            &mut window_store,
            &mut engine_memory,
            &VirtualWorkspaceSettings::default(),
            &LayoutSettings::default(),
        )
        .unwrap();
    let _ = std::fs::remove_file(path);

    assert!(!engine.persistence.windows.contains_key(&tiled));
    assert!(!engine.persistence.windows.contains_key(&floating));
    assert!(!engine.persistence.windows.contains_key(&out_of_scope));
    assert!(!engine.floating.is_floating(floating));
    assert_eq!(report.workspaces_replaced, 1);
    assert_eq!(report.unmatched, 2);
    assert_eq!(report.warnings, vec![RestoreWarning::UnmatchedWindows(2)]);
    assert_eq!(
        engine.floating_positions.get(space, target_workspace, floating),
        None,
    );
}

#[test]
fn workspace_restore_keeps_current_windows_absent_from_snapshot() {
    let space = SpaceId::new(129);
    let size = CGSize::new(1200.0, 800.0);
    let frame = objc2_core_foundation::CGRect::new(
        objc2_core_foundation::CGPoint::new(10.0, 20.0),
        CGSize::new(700.0, 500.0),
    );
    let saved = WindowId::new(70, 1);
    let live = WindowId::new(71, 1);
    let live_floating = WindowId::new(72, 1);

    let mut snapshot = test_engine();
    let mut snapshot_memory = DisplayMemory::default();
    let mut snapshot_store = WindowStore::default();
    let _ = snapshot.handle_event(
        &mut snapshot_store,
        &mut snapshot_memory,
        LayoutEvent::SpaceExposed(space, size),
    );
    let snapshot_workspace = snapshot.active_workspace(space).unwrap();
    let snapshot_layout = snapshot.workspace_layouts.active(space, snapshot_workspace).unwrap();
    snapshot
        .workspace_tree_mut(snapshot_workspace)
        .add_window_after_selection(snapshot_layout, saved);
    snapshot.persistence.windows.insert(
        saved,
        WindowFingerprint {
            window_server_id: Some(7001),
            title: Some("Saved".into()),
            width: 700.0,
            height: 500.0,
            app_id: Some("com.example.saved".into()),
        },
    );
    let path = std::env::temp_dir().join(format!(
        "rini-live-window-restore-test-{}-{}.ron",
        std::process::id(),
        space.get(),
    ));
    snapshot.save(&mut snapshot_memory, path.clone()).unwrap();

    let mut engine = test_engine();
    let mut engine_memory = DisplayMemory::default();

    let _memory = DisplayMemory::default();
    let mut window_store = WindowStore::default();
    let _ = engine.handle_event(
        &mut window_store,
        &mut engine_memory,
        LayoutEvent::SpaceExposed(space, size),
    );
    let target_workspace = engine.active_workspace(space).unwrap();
    let live_state = |title: &str, bundle_id: &str, window_server_id: u32| WindowState {
        info: WindowInfo {
            is_standard: true,
            is_root: true,
            is_minimized: false,
            is_resizable: true,
            min_size: None,
            max_size: None,
            title: title.into(),
            frame,
            sys_id: Some(WindowServerId::new(window_server_id)),
            bundle_id: Some(bundle_id.into()),
            path: None,
            ax_role: None,
            ax_subrole: None,
            is_modal: false,
        },
        frame_monotonic: frame,
        is_manageable: true,
        ignore_app_rule: false,
    };
    window_store.insert_window(live, live_state("Live", "com.example.live", 7101));
    window_store.insert_window(
        live_floating,
        live_state("Live floating", "com.example.live-floating", 7201),
    );
    for window in [live, live_floating] {
        assert!(engine.virtual_workspace_manager.assign_window_to_workspace(
            &mut window_store,
            space,
            window,
            target_workspace,
        ));
    }
    engine.add_window_to_layout(&mut window_store, &mut engine_memory, space, live);
    engine.floating.add_floating(live_floating);
    engine.floating_positions.store(space, target_workspace, live_floating, frame);
    engine.focused_window = Some(live);

    engine
        .restore_layout(
            path.clone(),
            RestoreRequest::new(RestoreScope::Workspace, space),
            &mut window_store,
            &mut engine_memory,
            &VirtualWorkspaceSettings::default(),
            &LayoutSettings::default(),
        )
        .unwrap();
    let _ = std::fs::remove_file(path);

    let target_layout = engine.workspace_layouts.active(space, target_workspace).unwrap();
    assert!(engine.workspace_tree(target_workspace).contains_window(target_layout, live));
    assert!(!engine.workspace_tree(target_workspace).contains_window(target_layout, saved));
    assert_eq!(
        window_store.workspace_for_window(space, live),
        Some(target_workspace)
    );
    assert!(engine.floating.is_floating(live_floating));
    assert_eq!(
        engine.floating_positions.get(space, target_workspace, live_floating),
        Some(frame)
    );
    assert_eq!(engine.focused_window, Some(live));
    assert_eq!(
        engine.virtual_workspace_manager.last_focused_window(space, target_workspace),
        Some(live)
    );
}

#[test]
fn workspace_restore_does_not_consume_live_window_from_sibling_workspace() {
    let space = SpaceId::new(132);
    let size = CGSize::new(1200.0, 800.0);
    let frame = objc2_core_foundation::CGRect::new(
        objc2_core_foundation::CGPoint::new(10.0, 20.0),
        CGSize::new(700.0, 500.0),
    );
    let saved = WindowId::new(74, 1);
    let live = WindowId::new(75, 1);

    let mut snapshot = test_engine();
    let mut snapshot_memory = DisplayMemory::default();
    let mut snapshot_store = WindowStore::default();
    let _ = snapshot.handle_event(
        &mut snapshot_store,
        &mut snapshot_memory,
        LayoutEvent::SpaceExposed(space, size),
    );
    let source_workspace = snapshot.active_workspace(space).unwrap();
    let source_layout = snapshot.workspace_layouts.active(space, source_workspace).unwrap();
    snapshot
        .workspace_tree_mut(source_workspace)
        .add_window_after_selection(source_layout, saved);
    snapshot.persistence.windows.insert(
        saved,
        WindowFingerprint {
            window_server_id: Some(7400),
            title: Some("Shared editor".into()),
            width: 700.0,
            height: 500.0,
            app_id: Some("com.example.editor".into()),
        },
    );
    let path = std::env::temp_dir().join(format!(
        "rini-sibling-workspace-restore-test-{}-{}.ron",
        std::process::id(),
        space.get(),
    ));
    snapshot.save(&mut snapshot_memory, path.clone()).unwrap();

    let mut engine = test_engine();
    let mut engine_memory = DisplayMemory::default();

    let _memory = DisplayMemory::default();
    let mut window_store = WindowStore::default();
    let _ = engine.handle_event(
        &mut window_store,
        &mut engine_memory,
        LayoutEvent::SpaceExposed(space, size),
    );
    let target_workspace = engine.active_workspace(space).unwrap();
    let sibling_workspace = engine
        .virtual_workspace_manager
        .existing_workspaces(space)
        .into_iter()
        .map(|(workspace, _)| workspace)
        .find(|workspace| *workspace != target_workspace)
        .unwrap();
    let sibling_layout = engine.workspace_layouts.active(space, sibling_workspace).unwrap();
    window_store.insert_window(
        live,
        WindowState {
            info: WindowInfo {
                is_standard: true,
                is_root: true,
                is_minimized: false,
                is_resizable: true,
                min_size: None,
                max_size: None,
                title: "Shared editor".into(),
                frame,
                sys_id: Some(WindowServerId::new(7400)),
                bundle_id: Some("com.example.editor".into()),
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
        live,
        sibling_workspace,
    ));
    engine
        .workspace_tree_mut(sibling_workspace)
        .add_window_after_selection(sibling_layout, live);

    let report = engine
        .restore_layout(
            path.clone(),
            RestoreRequest::new(RestoreScope::Workspace, space),
            &mut window_store,
            &mut engine_memory,
            &VirtualWorkspaceSettings::default(),
            &LayoutSettings::default(),
        )
        .unwrap();
    let _ = std::fs::remove_file(path);

    let target_layout = engine.workspace_layouts.active(space, target_workspace).unwrap();
    assert_eq!(report.matched, 0);
    assert_eq!(report.unmatched, 1);
    assert_eq!(
        window_store.workspace_for_window(space, live),
        Some(sibling_workspace)
    );
    assert!(engine.workspace_tree(sibling_workspace).contains_window(sibling_layout, live));
    assert!(!engine.workspace_tree(target_workspace).contains_window(target_layout, live));
    assert!(!engine.workspace_tree(target_workspace).contains_window(target_layout, saved));
}

#[test]
fn scoped_restore_does_not_consume_same_id_live_window_on_another_space() {
    let target_space = SpaceId::new(130);
    let external_space = SpaceId::new(131);
    let size = CGSize::new(1200.0, 800.0);
    let frame = objc2_core_foundation::CGRect::new(
        objc2_core_foundation::CGPoint::new(10.0, 20.0),
        CGSize::new(700.0, 500.0),
    );
    let reused_id = WindowId::new(73, 1);

    let mut snapshot = test_engine();
    let mut snapshot_memory = DisplayMemory::default();
    let mut snapshot_store = WindowStore::default();
    let _ = snapshot.handle_event(
        &mut snapshot_store,
        &mut snapshot_memory,
        LayoutEvent::SpaceExposed(target_space, size),
    );
    let snapshot_workspace = snapshot.active_workspace(target_space).unwrap();
    let snapshot_layout =
        snapshot.workspace_layouts.active(target_space, snapshot_workspace).unwrap();
    snapshot
        .workspace_tree_mut(snapshot_workspace)
        .add_window_after_selection(snapshot_layout, reused_id);
    snapshot.persistence.windows.insert(
        reused_id,
        WindowFingerprint {
            window_server_id: Some(7300),
            title: Some("Old saved window".into()),
            width: 700.0,
            height: 500.0,
            app_id: Some("com.example.old".into()),
        },
    );
    let path = std::env::temp_dir().join(format!(
        "rini-cross-space-id-collision-test-{}-{}.ron",
        std::process::id(),
        target_space.get(),
    ));
    snapshot.save(&mut snapshot_memory, path.clone()).unwrap();

    let mut engine = test_engine();
    let mut engine_memory = DisplayMemory::default();

    let _memory = DisplayMemory::default();
    let mut window_store = WindowStore::default();
    for space in [target_space, external_space] {
        let _ = engine.handle_event(
            &mut window_store,
            &mut engine_memory,
            LayoutEvent::SpaceExposed(space, size),
        );
    }
    let external_workspace = engine.active_workspace(external_space).unwrap();
    let external_layout =
        engine.workspace_layouts.active(external_space, external_workspace).unwrap();
    window_store.insert_window(
        reused_id,
        WindowState {
            info: WindowInfo {
                is_standard: true,
                is_root: true,
                is_minimized: false,
                is_resizable: true,
                min_size: None,
                max_size: None,
                title: "Current external window".into(),
                frame,
                sys_id: Some(WindowServerId::new(7310)),
                bundle_id: Some("com.example.current".into()),
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
        external_space,
        reused_id,
        external_workspace,
    ));
    engine
        .workspace_tree_mut(external_workspace)
        .add_window_after_selection(external_layout, reused_id);
    engine.floating.add_floating(reused_id);
    engine.floating.add_active(external_space, reused_id.pid, reused_id);

    engine
        .restore_layout(
            path.clone(),
            RestoreRequest::new(RestoreScope::Workspace, target_space),
            &mut window_store,
            &mut engine_memory,
            &VirtualWorkspaceSettings::default(),
            &LayoutSettings::default(),
        )
        .unwrap();
    let _ = std::fs::remove_file(path);

    let target_workspace = engine.active_workspace(target_space).unwrap();
    let target_layout = engine.workspace_layouts.active(target_space, target_workspace).unwrap();
    assert!(
        engine
            .workspace_tree(external_workspace)
            .contains_window(external_layout, reused_id)
    );
    assert!(
        !engine
            .workspace_tree(target_workspace)
            .contains_window(target_layout, reused_id)
    );
    assert_eq!(
        window_store.workspace_for_window(external_space, reused_id),
        Some(external_workspace)
    );
    assert!(engine.floating.is_floating(reused_id));
    assert!(engine.floating.active_flat(external_space).contains(&reused_id));
}

#[test]
fn space_restore_uses_workspace_assignment_over_stale_window_server_space() {
    let target_space = SpaceId::new(134);
    let external_space = SpaceId::new(135);
    let size = CGSize::new(1200.0, 800.0);
    let frame = objc2_core_foundation::CGRect::new(
        objc2_core_foundation::CGPoint::new(10.0, 20.0),
        CGSize::new(700.0, 500.0),
    );
    let saved = WindowId::new(77, 1);
    let live = WindowId::new(78, 1);
    let window_server_id = WindowServerId::new(7700);

    let mut snapshot = test_engine();
    let mut snapshot_memory = DisplayMemory::default();
    let mut snapshot_store = WindowStore::default();
    let _ = snapshot.handle_event(
        &mut snapshot_store,
        &mut snapshot_memory,
        LayoutEvent::SpaceExposed(target_space, size),
    );
    let source_workspace = snapshot.active_workspace(target_space).unwrap();
    let source_layout = snapshot.workspace_layouts.active(target_space, source_workspace).unwrap();
    snapshot
        .workspace_tree_mut(source_workspace)
        .add_window_after_selection(source_layout, saved);
    snapshot.persistence.windows.insert(
        saved,
        WindowFingerprint {
            window_server_id: Some(window_server_id.as_u32()),
            title: Some("External editor".into()),
            width: 700.0,
            height: 500.0,
            app_id: Some("com.example.editor".into()),
        },
    );
    let path = std::env::temp_dir().join(format!(
        "rini-stale-server-space-restore-test-{}-{}.ron",
        std::process::id(),
        target_space.get(),
    ));
    snapshot.save(&mut snapshot_memory, path.clone()).unwrap();

    let mut engine = test_engine();
    let mut engine_memory = DisplayMemory::default();

    let _memory = DisplayMemory::default();
    let mut window_store = WindowStore::default();
    for space in [target_space, external_space] {
        let _ = engine.handle_event(
            &mut window_store,
            &mut engine_memory,
            LayoutEvent::SpaceExposed(space, size),
        );
    }
    let external_workspace = engine.active_workspace(external_space).unwrap();
    let external_layout =
        engine.workspace_layouts.active(external_space, external_workspace).unwrap();
    window_store.insert_window(
        live,
        WindowState {
            info: WindowInfo {
                is_standard: true,
                is_root: true,
                is_minimized: false,
                is_resizable: true,
                min_size: None,
                max_size: None,
                title: "External editor".into(),
                frame,
                sys_id: Some(window_server_id),
                bundle_id: Some("com.example.editor".into()),
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
        external_space,
        live,
        external_workspace,
    ));
    engine
        .workspace_tree_mut(external_workspace)
        .add_window_after_selection(external_layout, live);
    // Model the transient that used to make space restore consume the external window.
    window_store.set_window_server_space(window_server_id, Some(target_space));

    let report = engine
        .restore_layout(
            path.clone(),
            RestoreRequest::new(RestoreScope::Space, target_space),
            &mut window_store,
            &mut engine_memory,
            &VirtualWorkspaceSettings::default(),
            &LayoutSettings::default(),
        )
        .unwrap();
    let _ = std::fs::remove_file(path);

    let target_workspace = engine.active_workspace(target_space).unwrap();
    let target_layout = engine.workspace_layouts.active(target_space, target_workspace).unwrap();
    assert_eq!(report.matched, 0);
    assert_eq!(report.unmatched, 1);
    assert_eq!(
        window_store.workspace_for_window(external_space, live),
        Some(external_workspace)
    );
    assert!(engine.workspace_tree(external_workspace).contains_window(external_layout, live));
    assert!(!engine.workspace_tree(target_workspace).contains_window(target_layout, live));
    assert!(!engine.workspace_tree(target_workspace).contains_window(target_layout, saved));
}

#[test]
fn space_restore_rejects_workspace_count_mismatch_before_mutating_layouts() {
    let space = SpaceId::new(125);
    let size = CGSize::new(1200.0, 800.0);
    let mut snapshot = test_engine();
    let mut memory = DisplayMemory::default();
    let mut snapshot_store = WindowStore::default();
    let _ = snapshot.handle_event(
        &mut snapshot_store,
        &mut memory,
        LayoutEvent::SpaceExposed(space, size),
    );
    let path = std::env::temp_dir().join(format!(
        "rini-space-count-restore-test-{}-{}.ron",
        std::process::id(),
        space.get(),
    ));
    snapshot.save(&mut memory, path.clone()).unwrap();

    let mut target_settings = VirtualWorkspaceSettings::default();
    target_settings.default_workspace_count = 3;
    let mut engine = LayoutEngine::new(&target_settings, &LayoutSettings::default());
    let mut memory = DisplayMemory::default();
    let mut window_store = WindowStore::default();
    let sentinel = WindowId::new(11, 1);
    let _ = engine.handle_event(
        &mut window_store,
        &mut memory,
        LayoutEvent::SpaceExposed(space, size),
    );
    let target_workspace = engine.active_workspace(space).unwrap();
    let target_layout = engine.workspace_layouts.active(space, target_workspace).unwrap();
    engine
        .workspace_tree_mut(target_workspace)
        .add_window_after_selection(target_layout, sentinel);

    let error = engine
        .restore_saved_layout(
            path.clone(),
            RestoreScope::Space,
            space,
            &mut window_store,
            &mut memory,
            &VirtualWorkspaceSettings::default(),
            &LayoutSettings::default(),
        )
        .unwrap_err();
    let _ = std::fs::remove_file(path);

    assert!(error.to_string().contains("different workspace counts"));
    assert!(
        engine.workspace_tree(target_workspace).contains_window(target_layout, sentinel),
        "a rejected restore must leave every existing workspace layout untouched"
    );
}

#[test]
fn saved_workspace_restore_uses_target_ordinal_and_preserves_configured_name() {
    let mut workspace_settings = VirtualWorkspaceSettings::default();
    workspace_settings.default_workspace_count = 6;
    workspace_settings.workspace_names =
        ["B", "C", "E", "T", "X", "S"].into_iter().map(str::to_owned).collect();
    workspace_settings.default_workspace = 3;
    let layout_settings = LayoutSettings::default();
    let space = SpaceId::new(609);
    let size = CGSize::new(1200.0, 800.0);
    let mut snapshot = LayoutEngine::new(&workspace_settings, &layout_settings);
    let mut memory = DisplayMemory::default();
    let mut snapshot_store = WindowStore::default();
    let _ = snapshot.handle_event(
        &mut snapshot_store,
        &mut memory,
        LayoutEvent::SpaceExposed(space, size),
    );
    let saved_workspaces = snapshot.virtual_workspace_manager.existing_workspaces(space);
    let saved_t = saved_workspaces[3].0;
    let saved_s = saved_workspaces[5].0;
    assert_eq!(snapshot.active_workspace(space), Some(saved_t));
    // Names rather than layout modes as the marker; see the note in
    // portable_restore_uses_the_space_that_was_active_when_saved.
    assert!(snapshot.virtual_workspace_manager.rename_workspace(
        space,
        saved_t,
        "saved-t".to_string(),
    ));
    assert!(snapshot.virtual_workspace_manager.rename_workspace(
        space,
        saved_s,
        "saved-s".to_string(),
    ));
    let path = std::env::temp_dir().join(format!(
        "rini-saved-workspace-ordinal-test-{}-{}.ron",
        std::process::id(),
        space.get(),
    ));
    snapshot
        .save_current_layout(path.clone(), &snapshot_store, &mut memory, Some(space))
        .unwrap();

    let mut engine = LayoutEngine::new(&workspace_settings, &layout_settings);

    let mut memory = DisplayMemory::default();
    let mut window_store = WindowStore::default();
    let _ = engine.handle_event(
        &mut window_store,
        &mut memory,
        LayoutEvent::SpaceExposed(space, size),
    );
    let target_s = engine.virtual_workspace_manager.existing_workspaces(space)[5].0;
    assert!(engine.virtual_workspace_manager.set_active_workspace(space, target_s));

    engine
        .restore_layout(
            path.clone(),
            RestoreRequest::from_saved_file(RestoreScope::Workspace, space),
            &mut window_store,
            &mut memory,
            &workspace_settings,
            &layout_settings,
        )
        .unwrap();
    let _ = std::fs::remove_file(path);

    let restored = engine.virtual_workspace_manager.workspace_info(space, target_s).unwrap();
    assert_eq!(restored.name, "S");
}

#[test]
fn portable_restore_rejects_ambiguous_legacy_multi_space_files() {
    let source_a = SpaceId::new(603);
    let source_b = SpaceId::new(604);
    let target_space = SpaceId::new(605);
    let size = CGSize::new(1200.0, 800.0);
    let mut snapshot = test_engine();
    let mut snapshot_memory = DisplayMemory::default();
    let mut snapshot_store = WindowStore::default();
    for space in [source_a, source_b] {
        let _ = snapshot.handle_event(
            &mut snapshot_store,
            &mut snapshot_memory,
            LayoutEvent::SpaceExposed(space, size),
        );
    }
    // A direct snapshot models a legacy file, which has no saved-active-space hint.
    let path = std::env::temp_dir().join(format!(
        "rini-ambiguous-portable-restore-test-{}-{}.ron",
        std::process::id(),
        target_space.get(),
    ));
    snapshot.save(&mut snapshot_memory, path.clone()).unwrap();

    let mut engine = test_engine();
    let mut engine_memory = DisplayMemory::default();

    let _memory = DisplayMemory::default();
    let mut window_store = WindowStore::default();
    let _ = engine.handle_event(
        &mut window_store,
        &mut engine_memory,
        LayoutEvent::SpaceExposed(target_space, size),
    );
    let target_workspace = engine.active_workspace(target_space).unwrap();
    let before_name = engine
        .virtual_workspace_manager
        .workspace_info(target_space, target_workspace)
        .unwrap()
        .name
        .clone();

    let error = engine
        .restore_layout(
            path.clone(),
            RestoreRequest::new(RestoreScope::Workspace, target_space),
            &mut window_store,
            &mut engine_memory,
            &VirtualWorkspaceSettings::default(),
            &LayoutSettings::default(),
        )
        .unwrap_err();
    let _ = std::fs::remove_file(path);

    assert!(error.to_string().contains("cannot choose a source"), "{error}");
    assert_eq!(
        engine
            .virtual_workspace_manager
            .workspace_info(target_space, target_workspace)
            .unwrap()
            .name,
        before_name,
    );
}

#[test]
fn portable_restore_uses_the_space_that_was_active_when_saved() {
    let source_a = SpaceId::new(601);
    let source_b = SpaceId::new(602);
    // The target id also exists in the file. Portable restore must still use the saved origin,
    // while saved-file restore deliberately prefers this matching current-space entry.
    let target_space = source_a;
    let size = CGSize::new(1200.0, 800.0);
    let mut snapshot = test_engine();
    let mut snapshot_memory = DisplayMemory::default();
    let mut snapshot_store = WindowStore::default();
    for space in [source_a, source_b] {
        let _ = snapshot.handle_event(
            &mut snapshot_store,
            &mut snapshot_memory,
            LayoutEvent::SpaceExposed(space, size),
        );
    }
    let source_a_workspace = snapshot.active_workspace(source_a).unwrap();
    let source_b_workspace = snapshot.active_workspace(source_b).unwrap();
    // These used to be given DIFFERENT layout modes so the assertions could tell which
    // snapshot won. With one mode left, name them instead — same purpose, and it survives
    // the layout modes being removed.
    assert!(snapshot.virtual_workspace_manager.rename_workspace(
        source_a,
        source_a_workspace,
        "saved-a".to_string(),
    ));
    assert!(snapshot.virtual_workspace_manager.rename_workspace(
        source_b,
        source_b_workspace,
        "saved-b".to_string(),
    ));
    let path = std::env::temp_dir().join(format!(
        "rini-portable-source-restore-test-{}-{}.ron",
        std::process::id(),
        target_space.get(),
    ));
    snapshot
        .save_current_layout(
            path.clone(),
            &snapshot_store,
            &mut snapshot_memory,
            Some(source_b),
        )
        .unwrap();

    let mut engine = test_engine();
    let mut engine_memory = DisplayMemory::default();

    let _memory = DisplayMemory::default();
    let mut window_store = WindowStore::default();
    let _ = engine.handle_event(
        &mut window_store,
        &mut engine_memory,
        LayoutEvent::SpaceExposed(target_space, size),
    );
    let target_workspace = engine.active_workspace(target_space).unwrap();
    let target_name = engine
        .virtual_workspace_manager
        .workspace_info(target_space, target_workspace)
        .unwrap()
        .name
        .clone();
    engine
        .restore_layout(
            path.clone(),
            RestoreRequest::new(RestoreScope::Workspace, target_space),
            &mut window_store,
            &mut engine_memory,
            &VirtualWorkspaceSettings::default(),
            &LayoutSettings::default(),
        )
        .unwrap();

    assert_eq!(
        engine
            .virtual_workspace_manager
            .workspace_info(target_space, target_workspace)
            .unwrap()
            .name,
        target_name,
    );

    let mut saved_target = test_engine();
    let mut saved_target_memory = DisplayMemory::default();
    let mut saved_store = WindowStore::default();
    let _ = saved_target.handle_event(
        &mut saved_store,
        &mut saved_target_memory,
        LayoutEvent::SpaceExposed(target_space, size),
    );
    let saved_workspace = saved_target.active_workspace(target_space).unwrap();
    saved_target
        .restore_layout(
            path.clone(),
            RestoreRequest::from_saved_file(RestoreScope::Workspace, target_space),
            &mut saved_store,
            &mut saved_target_memory,
            &VirtualWorkspaceSettings::default(),
            &LayoutSettings::default(),
        )
        .unwrap();
    let _ = std::fs::remove_file(path);
    assert_eq!(
        saved_target
            .virtual_workspace_manager
            .workspace_info(target_space, saved_workspace)
            .unwrap()
            .name,
        target_name,
    );
}

#[test]
fn master_restore_resolves_old_space_id_by_display_identity() {
    let saved_a = SpaceId::new(630);
    let saved_b = SpaceId::new(631);
    let current_a = SpaceId::new(632);
    let size = CGSize::new(1200.0, 800.0);
    let mut snapshot = test_engine();
    let mut snapshot_memory = DisplayMemory::default();
    let mut snapshot_store = WindowStore::default();
    for (space, display) in [(saved_a, "display-a"), (saved_b, "display-b")] {
        let _ = snapshot.handle_event(
            &mut snapshot_store,
            &mut snapshot_memory,
            LayoutEvent::SpaceExposed(space, size),
        );
        snapshot.update_space_display(&mut snapshot_memory, space, Some(display.into()));
        let workspace = snapshot.active_workspace(space).unwrap();
        assert!(snapshot.virtual_workspace_manager.rename_workspace(
            space,
            workspace,
            format!("saved-{display}"),
        ));
    }
    let path = std::env::temp_dir().join(format!(
        "rini-saved-display-source-test-{}.ron",
        std::process::id(),
    ));
    snapshot
        .save_current_layout(
            path.clone(),
            &snapshot_store,
            &mut snapshot_memory,
            Some(saved_b),
        )
        .unwrap();

    let mut engine = test_engine();
    let mut engine_memory = DisplayMemory::default();

    let _memory = DisplayMemory::default();
    let mut window_store = WindowStore::default();
    let _ = engine.handle_event(
        &mut window_store,
        &mut engine_memory,
        LayoutEvent::SpaceExposed(current_a, size),
    );
    engine.update_space_display(&mut engine_memory, current_a, Some("display-a".into()));
    engine
        .restore_layout(
            path.clone(),
            RestoreRequest::from_saved_file(RestoreScope::Workspace, current_a),
            &mut window_store,
            &mut engine_memory,
            &VirtualWorkspaceSettings::default(),
            &LayoutSettings::default(),
        )
        .unwrap();
    let _ = std::fs::remove_file(path);
}

#[test]
fn runtime_restore_cleans_unmatched_windows_from_inactive_size_configurations() {
    let space = SpaceId::new(504);
    let small = CGSize::new(1200.0, 800.0);
    let large = CGSize::new(1600.0, 1000.0);
    let ghost = WindowId::new(33419, 82684);
    let mut snapshot = test_engine();
    let mut snapshot_memory = DisplayMemory::default();
    let mut snapshot_store = WindowStore::default();
    let _ = snapshot.handle_event(
        &mut snapshot_store,
        &mut snapshot_memory,
        LayoutEvent::SpaceExposed(space, large),
    );
    let workspace = snapshot.active_workspace(space).unwrap();
    let large_layout = snapshot.workspace_layouts.active(space, workspace).unwrap();
    let small_layout = snapshot.virtual_workspace_manager.workspaces[workspace]
        .layout_system
        .create_layout();
    snapshot.workspace_layouts.insert_layout_configuration_for_test(
        space,
        workspace,
        small,
        small_layout,
    );
    assert_ne!(small_layout, large_layout);
    snapshot
        .workspace_tree_mut(workspace)
        .add_window_after_selection(small_layout, ghost);
    snapshot.persistence.windows.insert(
        ghost,
        WindowFingerprint {
            window_server_id: Some(82684),
            title: Some("Music".into()),
            width: 1512.0,
            height: 944.0,
            app_id: Some("com.apple.Music".into()),
        },
    );
    let path = std::env::temp_dir().join(format!(
        "rini-runtime-inactive-size-restore-test-{}-{}.ron",
        std::process::id(),
        space.get(),
    ));
    snapshot.save(&mut snapshot_memory, path.clone()).unwrap();

    let mut engine = test_engine();
    let mut engine_memory = DisplayMemory::default();

    let _memory = DisplayMemory::default();
    let mut window_store = WindowStore::default();
    let _ = engine.handle_event(
        &mut window_store,
        &mut engine_memory,
        LayoutEvent::SpaceExposed(space, large),
    );
    let report = engine
        .restore_layout(
            path.clone(),
            RestoreRequest::new(RestoreScope::Space, space),
            &mut window_store,
            &mut engine_memory,
            &VirtualWorkspaceSettings::default(),
            &LayoutSettings::default(),
        )
        .unwrap();
    let _ = std::fs::remove_file(path);

    assert_eq!(report.unmatched, 1);
    for (_, restored_workspace, layout) in engine.workspace_layouts.all_layouts() {
        assert!(
            !engine.workspace_tree(restored_workspace).contains_window(layout, ghost),
            "unmatched runtime-restore candidate survived in a dormant size configuration"
        );
    }
    assert!(!engine.persistence.windows.contains_key(&ghost));
}
