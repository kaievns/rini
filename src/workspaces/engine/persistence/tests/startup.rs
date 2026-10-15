//! Restoring at launch, where nothing is running yet and macOS has renumbered the spaces.

use objc2_core_foundation::CGSize;

use super::*;
use crate::workspaces::LayoutEvent;
use crate::workspaces::domain::display_memory::DisplayMemory;

#[test]
fn startup_validation_preserves_stale_ids_when_the_app_can_still_fuzzy_match() {
    // Restored state drops apps that are no longer running: `closed` below is that case.
    let mut engine = test_engine();
    let mut memory = DisplayMemory::default();
    let mut window_store = WindowStore::default();
    let space = SpaceId::new(129);
    let closed = WindowId::new(33419, 82684);
    let still_open = WindowId::new(1430, 97361);
    let restarted_app = WindowId::new(40000, 70000);
    let _ = engine.handle_event(
        &mut window_store,
        &mut memory,
        LayoutEvent::SpaceExposed(space, CGSize::new(1200.0, 800.0)),
    );
    for window in [closed, still_open, restarted_app] {
        let _ = engine.handle_event(
            &mut window_store,
            &mut memory,
            LayoutEvent::WindowAdded(space, window),
        );
        engine.persistence.windows.insert(window, WindowFingerprint {
            window_server_id: Some(window.idx.get()),
            title: Some(format!("window-{}", window.idx.get())),
            width: 600.0,
            height: 800.0,
            app_id: Some(format!("com.example.{}", window.pid)),
        });
        engine.persistence.pending_windows.insert(window);
    }

    let discarded = engine.discard_unmatchable_startup_candidates(
        |window, id| window.pid == still_open.pid && id == still_open.idx.get(),
        |app_id| app_id == format!("com.example.{}", restarted_app.pid),
    );
    let workspace = engine.active_workspace(space).unwrap();
    let layout = engine.workspace_layouts.active(space, workspace).unwrap();

    assert_eq!(discarded, 1);
    assert!(!engine.workspace_tree(workspace).contains_window(layout, closed));
    assert!(!engine.persistence.windows.contains_key(&closed));
    assert!(engine.workspace_tree(workspace).contains_window(layout, still_open));
    assert!(engine.persistence.pending_windows.contains(&still_open));
    assert!(engine.workspace_tree(workspace).contains_window(layout, restarted_app));
    assert!(engine.persistence.pending_windows.contains(&restarted_app));
}

/// A ghost saved in the layout of a display size not showing goes from every layout when its app's
/// discovery completes, not only from the one on screen.
#[test]
fn completed_app_discovery_discards_a_ghost_from_a_dormant_display_size() {
    let space = SpaceId::new(504);
    let small = CGSize::new(1200.0, 800.0);
    let large = CGSize::new(1600.0, 1000.0);
    let ghost = WindowId::new(33419, 82684);
    let mut snapshot = test_engine();
    let mut memory = DisplayMemory::default();
    let mut snapshot_store = WindowStore::default();
    let _ = snapshot.handle_event(
        &mut snapshot_store,
        &mut memory,
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
    snapshot.persistence.windows.insert(ghost, WindowFingerprint {
        window_server_id: Some(82684),
        title: Some("Music".into()),
        width: 1512.0,
        height: 944.0,
        app_id: Some("com.apple.Music".into()),
    });
    let (_dir, path) = layout_file();
    snapshot.save(&memory, path.clone()).unwrap();

    let mut engine = LayoutEngine::load(path.clone()).unwrap();
    let mut window_store = WindowStore::default();
    let _ = engine.handle_event(
        &mut window_store,
        &mut memory,
        LayoutEvent::SpaceExposed(space, large),
    );
    assert!(
        engine.persistence.pending_windows.contains(&ghost),
        "fixture: a candidate"
    );

    let _ = engine.handle_event(
        &mut window_store,
        &mut memory,
        LayoutEvent::WindowDiscoveryCompleted(ghost.pid, None, vec![space]),
    );

    for (_, workspace, layout) in engine.workspace_layouts.all_layouts() {
        assert!(!engine.workspace_tree(workspace).contains_window(layout, ghost));
    }
    assert!(!engine.persistence.windows.contains_key(&ghost));
    assert!(!engine.persistence.pending_windows.contains(&ghost));
}

#[test]
fn completed_app_discovery_discards_unmatched_startup_ghosts() {
    let mut engine = test_engine();
    let mut memory = DisplayMemory::default();
    let mut window_store = WindowStore::default();
    let space = SpaceId::new(126);
    let ghost = WindowId::new(55, 1);
    let inactive_space = SpaceId::new(128);
    let inactive_ghost = WindowId::new(55, 2);
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
    engine.persistence.windows.insert(ghost, WindowFingerprint {
        window_server_id: Some(9000),
        title: Some("Closed window".into()),
        width: 800.0,
        height: 600.0,
        app_id: Some("com.example.closed-window".into()),
    });
    engine.persistence.pending_windows.insert(ghost);
    let _ = engine.handle_event(
        &mut window_store,
        &mut memory,
        LayoutEvent::SpaceExposed(inactive_space, CGSize::new(1200.0, 800.0)),
    );
    let _ = engine.handle_event(
        &mut window_store,
        &mut memory,
        LayoutEvent::WindowAdded(inactive_space, inactive_ghost),
    );
    engine.persistence.windows.insert(inactive_ghost, WindowFingerprint {
        window_server_id: Some(9001),
        title: Some("Inactive-space window".into()),
        width: 800.0,
        height: 600.0,
        app_id: Some("com.example.closed-window".into()),
    });
    engine.persistence.pending_windows.insert(inactive_ghost);
    let workspace = engine.active_workspace(space).unwrap();
    let layout = engine.workspace_layouts.active(space, workspace).unwrap();
    engine.focused_window = Some(ghost);
    engine
        .virtual_workspace_manager
        .set_last_focused_window(space, workspace, Some(ghost));
    engine.floating.add_floating(ghost);
    engine.floating.set_last_focus(Some(ghost));
    assert!(engine.workspace_tree(workspace).contains_window(layout, ghost));

    let _ = engine.handle_event(
        &mut window_store,
        &mut memory,
        LayoutEvent::WindowDiscoveryCompleted(ghost.pid, None, vec![space]),
    );

    assert!(!engine.workspace_tree(workspace).contains_window(layout, ghost));
    assert!(!engine.persistence.windows.contains_key(&ghost));
    assert!(!engine.persistence.pending_windows.contains(&ghost));
    assert_eq!(engine.focused_window, None);
    assert_eq!(
        engine.virtual_workspace_manager.last_focused_window(space, workspace),
        None
    );
    assert!(!engine.floating.is_floating(ghost));
    assert_ne!(engine.floating.last_focus(), Some(ghost));
    assert!(engine.persistence.pending_windows.contains(&inactive_ghost));
    assert!(engine.restored_location_for_window(inactive_ghost).is_some());
}

#[test]
fn startup_restore_reapplies_configured_workspace_names() {
    let saved_settings = VirtualWorkspaceSettings {
        workspace_names: vec!["Old A".into(), "Old B".into()],
        ..VirtualWorkspaceSettings::default()
    };
    let layout_settings = LayoutSettings::default();
    let space = SpaceId::new(608);
    let mut snapshot = LayoutEngine::new(&saved_settings, &layout_settings);
    let mut memory = DisplayMemory::default();
    let mut window_store = WindowStore::default();
    let _ = snapshot.handle_event(
        &mut window_store,
        &mut memory,
        LayoutEvent::SpaceExposed(space, CGSize::new(1200.0, 800.0)),
    );
    let mut restored =
        LayoutEngine::deserialize_from_str(&snapshot.serialize_to_string(&memory)).unwrap();
    let mut current_settings = saved_settings;
    current_settings.workspace_names = vec!["A".into(), "S".into()];

    restored.finish_loading(&current_settings, &layout_settings);

    let names = restored
        .virtual_workspace_manager
        .existing_workspaces()
        .into_iter()
        .map(|(_, name)| name)
        .collect::<Vec<_>>();
    assert_eq!(names, ["A", "S"]);
}

#[test]
fn startup_restore_remaps_saved_space_by_display_identity_once() {
    let saved_space = SpaceId::new(610);
    let current_space = SpaceId::new(611);
    let later_space = SpaceId::new(612);
    let size = CGSize::new(1200.0, 800.0);
    let display = "display-a".to_string();
    let mut snapshot = test_engine();
    let mut memory = DisplayMemory::default();
    let mut snapshot_store = WindowStore::default();
    let _ = snapshot.handle_event(
        &mut snapshot_store,
        &mut memory,
        LayoutEvent::SpaceExposed(saved_space, size),
    );
    snapshot.update_space_display(&mut memory, saved_space, Some(display.clone()));
    let (_dir, path) = layout_file();
    snapshot.save(&memory, path.clone()).unwrap();

    let (mut restored, _restored_memory) =
        LayoutEngine::load_for_startup_restore(path.clone(), |_, _| false, |_| false).unwrap();
    let mut window_store = WindowStore::default();
    // An incomplete first topology snapshot must not consume the one-shot reconciliation.
    restored.reconcile_startup_spaces(&mut window_store, &mut memory, &[], 1);
    restored.reconcile_startup_spaces(
        &mut window_store,
        &mut memory,
        &[(current_space, display.clone())],
        1,
    );

    assert!(!restored.workspace_layouts.spaces().contains(&saved_space));
    assert!(restored.workspace_layouts.spaces().contains(&current_space));

    // The repair is startup-only. A later ordinary native-space switch must not migrate state.
    restored.reconcile_startup_spaces(&mut window_store, &mut memory, &[(later_space, display)], 1);
    assert!(restored.workspace_layouts.spaces().contains(&current_space));
    assert!(!restored.workspace_layouts.spaces().contains(&later_space));
}

/// Two displays swapping native space ids must keep each display's own strip.
///
/// Workspaces are global, so both displays share one. What needs to survive a swap is
/// per-display state: which display shows which workspace, and which windows are on each
/// display's strip.
#[test]
fn startup_restore_handles_space_id_swaps_between_displays() {
    let space_a = SpaceId::new(620);
    let space_b = SpaceId::new(621);
    let size = CGSize::new(1200.0, 800.0);
    let on_a = WindowId::new(10, 1);
    let on_b = WindowId::new(11, 1);

    let mut snapshot = test_engine();
    let mut memory = DisplayMemory::default();
    let mut snapshot_store = WindowStore::default();
    for (space, display, window) in [(space_a, "display-a", on_a), (space_b, "display-b", on_b)] {
        let _ = snapshot.handle_event(
            &mut snapshot_store,
            &mut memory,
            LayoutEvent::SpaceExposed(space, size),
        );
        snapshot.update_space_display(&mut memory, space, Some(display.into()));
        let _ = snapshot.handle_event(
            &mut snapshot_store,
            &mut memory,
            LayoutEvent::WindowAdded(space, window),
        );
        snapshot.persistence.windows.insert(window, WindowFingerprint {
            window_server_id: Some(window.idx.get()),
            title: Some(display.to_string()),
            width: 600.0,
            height: 400.0,
            app_id: Some("com.example.app".into()),
        });
    }
    // Each display's strip holds its own window before the swap.
    let workspace = snapshot.active_workspace(space_a).unwrap();
    assert_eq!(
        snapshot
            .virtual_workspace_manager
            .workspace_windows(&snapshot_store, space_a, workspace),
        vec![on_a]
    );

    let (_dir, path) = layout_file();
    snapshot.save(&memory, path.clone()).unwrap();

    let (mut restored, _restored_memory) =
        LayoutEngine::load_for_startup_restore(path.clone(), |_, _| false, |_| false).unwrap();
    let mut window_store = WindowStore::default();
    // display-a comes back as space_b and vice versa.
    restored.reconcile_startup_spaces(
        &mut window_store,
        &mut memory,
        &[(space_b, "display-a".into()), (space_a, "display-b".into())],
        2,
    );

    assert_eq!(
        memory.affinity.space_for_display("display-a"),
        Some(space_b),
        "display-a must now be recorded on the space id it came back as"
    );
    assert_eq!(
        memory.affinity.space_for_display("display-b"),
        Some(space_a),
        "and display-b on the other, without the two being confused"
    );
    assert!(
        restored.active_workspace(space_a).is_some()
            && restored.active_workspace(space_b).is_some(),
        "both displays keep showing a workspace across the swap"
    );
}

/// Restoring a layout must never leave windows on a display that is not attached.
///
/// This is what made restore unsafe to enable by default. A saved layout describes every
/// display the last session had, so restoring it while the external is unplugged put its
/// windows at that display's coordinates — measured at x=-1680, with nothing there — and
/// no path migrated them back. One dock/undock cycle produced a layout that could only be
/// fixed by deleting ~/.rini/layout.ron.
#[test]
fn startup_restore_releases_windows_saved_on_an_absent_display() {
    let builtin_space = SpaceId::new(630);
    let external_space = SpaceId::new(631);
    let size = CGSize::new(1200.0, 800.0);
    let builtin = "display-builtin".to_string();
    let external = "display-external".to_string();

    let mut snapshot = test_engine();
    let mut memory = DisplayMemory::default();
    let mut snapshot_store = WindowStore::default();
    let on_builtin = WindowId::new(10, 1);
    let on_external = WindowId::new(10, 2);
    for (space, display, window, server_id) in [
        (builtin_space, &builtin, on_builtin, 5001u32),
        (external_space, &external, on_external, 5002u32),
    ] {
        let _ = snapshot.handle_event(
            &mut snapshot_store,
            &mut memory,
            LayoutEvent::SpaceExposed(space, size),
        );
        snapshot.update_space_display(&mut memory, space, Some(display.clone()));
        let _ = snapshot.handle_event(
            &mut snapshot_store,
            &mut memory,
            LayoutEvent::WindowAdded(space, window),
        );
        snapshot.persistence.windows.insert(window, WindowFingerprint {
            window_server_id: Some(server_id),
            title: Some(format!("window-{server_id}")),
            width: 600.0,
            height: 400.0,
            app_id: Some("com.example.app".into()),
        });
    }
    let (_dir, path) = layout_file();
    snapshot.save(&memory, path.clone()).unwrap();

    let mut restored = LayoutEngine::load(path.clone()).unwrap();
    restored.startup_restore_pending = true;
    assert!(
        restored.restored_location_for_window(on_external).is_some(),
        "the saved file must contain a slot for the window on the external"
    );

    // Forget every recorded home first, so the release path itself has to establish the home
    // from the saved space's display. Without this the assertion below would pass on affinity
    // that had merely been carried over from the save.
    for window in [on_builtin, on_external] {
        memory.affinity.forget_window(window);
    }
    assert_eq!(memory.affinity.window_home(on_external), None);

    // Boot with ONLY the built-in attached, as after undocking.
    let mut window_store = WindowStore::default();
    restored.reconcile_startup_spaces(
        &mut window_store,
        &mut memory,
        &[(builtin_space, builtin.clone())],
        1,
    );

    assert!(
        restored.restored_location_for_window(on_external).is_none(),
        "a window saved on an unplugged display must not keep a saved slot there; \
         restoring it strands the window off-screen with no display to show it"
    );
    assert_eq!(
        memory.affinity.window_home(on_external),
        Some(external.as_str()),
        "its display affinity must survive, so plugging the display back in returns it"
    );
    assert!(
        restored.restored_location_for_window(on_builtin).is_some(),
        "a window on the display that IS attached must keep its saved size and position"
    );
    assert_eq!(
        memory.affinity.window_home(on_builtin),
        None,
        "a window on an attached display keeps its saved slot, so the release path must \
         leave it alone entirely"
    );
}

/// Startup restore asks only the two reads it is handed whether a saved window is still there: a
/// window the window server still has, or one whose application runs, is kept; one neither finds
/// is dropped.
#[test]
fn startup_restore_keeps_the_saved_windows_its_reads_still_find() {
    let space = SpaceId::new(630);
    let (gone, open, restarted) =
        (WindowId::new(50, 1), WindowId::new(51, 2), WindowId::new(52, 3));
    let mut snapshot = test_engine();
    let mut memory = DisplayMemory::default();
    let mut store = WindowStore::default();
    let _ = snapshot.handle_event(
        &mut store,
        &mut memory,
        LayoutEvent::SpaceExposed(space, CGSize::new(1200.0, 800.0)),
    );
    for window in [gone, open, restarted] {
        let _ =
            snapshot.handle_event(&mut store, &mut memory, LayoutEvent::WindowAdded(space, window));
        snapshot.persistence.windows.insert(window, WindowFingerprint {
            window_server_id: Some(window.idx.get()),
            title: None,
            width: 600.0,
            height: 400.0,
            app_id: Some(format!("com.example.{}", window.pid)),
        });
    }
    let (_dir, path) = layout_file();
    snapshot.save(&memory, path.clone()).unwrap();

    let (restored, _) = LayoutEngine::load_for_startup_restore(
        path.clone(),
        |window, id| window == open && id == open.idx.get(),
        |app| app == "com.example.52",
    )
    .unwrap();

    let saved = |window| restored.persistence.windows.contains_key(&window);
    assert_eq!((saved(gone), saved(open), saved(restarted)), (false, true, true));
}
