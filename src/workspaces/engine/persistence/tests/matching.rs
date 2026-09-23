//! Which live window a saved identity is, and which ones it must not be.
//!
//! `WindowId { pid, idx }` dies with the process and `WindowServerId` is recycled, so a saved identity
//! matching the wrong live window is the failure every rule here guards against.

use crate::workspaces::domain::display_memory::DisplayMemory;
use objc2_core_foundation::CGSize;

use super::*;
use crate::windows::domain::info::WindowInfo;
use crate::windows::domain::state::WindowState;
use crate::workspaces::LayoutEvent;
use rini_core::ids::WindowServerId;

#[test]
fn identity_transfer_preserves_window_tree_position_and_fingerprint() {
    let mut window_store = WindowStore::default();
    let mut engine = test_engine();
    let mut memory = DisplayMemory::default();
    let space = SpaceId::new(77);
    let old = WindowId::new(10, 1);
    let sibling = WindowId::new(10, 2);
    let replacement = WindowId::new(20, 9);

    let _ = engine.handle_event(
        &mut window_store,
        &mut memory,
        LayoutEvent::SpaceExposed(space, CGSize::new(1200.0, 800.0)),
    );
    let _ = engine.handle_event(
        &mut window_store,
        &mut memory,
        LayoutEvent::WindowAdded(space, old),
    );
    let _ = engine.handle_event(
        &mut window_store,
        &mut memory,
        LayoutEvent::WindowAdded(space, sibling),
    );
    engine.persistence.windows.insert(
        old,
        WindowFingerprint {
            window_server_id: Some(42),
            title: Some("Editor".into()),
            width: 800.0,
            height: 600.0,
            app_id: Some("com.example.editor".into()),
        },
    );
    engine.persistence.pending_windows.insert(old);
    let workspace = engine.active_workspace(space).unwrap();
    let layout = engine.workspace_layouts.active(space, workspace).unwrap();
    let other_workspace = engine
        .virtual_workspace_manager
        .list_workspaces(space)
        .into_iter()
        .map(|(workspace, _)| workspace)
        .find(|candidate| *candidate != workspace)
        .unwrap();
    let other_layout = engine.workspace_layouts.active(space, other_workspace).unwrap();
    // Model a provisional live projection created before the restored identity is matched.
    engine
        .workspace_tree_mut(other_workspace)
        .add_window_after_selection(other_layout, replacement);
    let before = engine.workspace_tree(workspace).visible_windows_in_layout(layout);

    engine.transfer_persistent_window_identity(&mut memory, old, replacement);

    let after = engine.workspace_tree(workspace).visible_windows_in_layout(layout);
    assert_eq!(
        after,
        before
            .into_iter()
            .map(|window| if window == old { replacement } else { window })
            .collect::<Vec<_>>()
    );
    assert!(!engine.persistence.windows.contains_key(&old));
    assert_eq!(
        engine.persistence.windows[&replacement].window_server_id,
        Some(42)
    );
    assert!(!engine.persistence.pending_windows.contains(&old));
    assert!(engine.persistence.pending_windows.contains(&replacement));
    assert!(
        !engine
            .workspace_tree(other_workspace)
            .contains_window(other_layout, replacement),
        "identity replacement must not leave the live id in its provisional workspace"
    );
}

#[test]
fn pure_matcher_reports_duplicate_identities_without_mutating_candidates() {
    use super::matcher::{RestoreCandidate, choose_match};

    let stale = WindowId::new(1, 1);
    let preferred = WindowId::new(1, 2);
    let live = WindowId::new(2, 1);
    let space = SpaceId::new(500);
    let stale_workspace = crate::workspaces::VirtualWorkspaceId::default();
    let preferred_workspace = crate::workspaces::VirtualWorkspaceId::default();
    let fingerprint = WindowFingerprint {
        window_server_id: Some(77),
        title: Some("Editor".into()),
        width: 800.0,
        height: 600.0,
        app_id: Some("com.example.editor".into()),
    };
    let candidates = vec![
        RestoreCandidate {
            window: stale,
            fingerprint: &fingerprint,
            location: Some((SpaceId::new(499), stale_workspace)),
        },
        RestoreCandidate {
            window: preferred,
            fingerprint: &fingerprint,
            location: Some((space, preferred_workspace)),
        },
    ];

    let decision = choose_match(
        live,
        space,
        &fingerprint,
        Some((space, preferred_workspace)),
        &candidates,
    )
    .unwrap();

    assert_eq!(decision.selected, preferred);
    assert_eq!(decision.duplicate_identities, vec![stale]);
    assert_eq!(candidates.len(), 2);
}

#[test]
fn reused_process_local_identity_defers_to_window_server_identity() {
    use super::matcher::{RestoreCandidate, choose_match};

    let live = WindowId::new(42, 7);
    let other = WindowId::new(42, 8);
    let space = SpaceId::new(501);
    let workspace = crate::workspaces::VirtualWorkspaceId::default();
    let direct_fingerprint = WindowFingerprint {
        window_server_id: Some(10),
        title: Some("Direct".into()),
        width: 600.0,
        height: 800.0,
        app_id: Some("com.example.editor".into()),
    };
    let other_fingerprint = WindowFingerprint {
        window_server_id: Some(20),
        title: Some("Other".into()),
        width: 600.0,
        height: 800.0,
        app_id: Some("com.example.editor".into()),
    };
    let live_fingerprint = WindowFingerprint {
        window_server_id: Some(20),
        title: Some("Other".into()),
        width: 600.0,
        height: 800.0,
        app_id: Some("com.example.editor".into()),
    };
    let candidates = [
        RestoreCandidate {
            window: live,
            fingerprint: &direct_fingerprint,
            location: Some((space, workspace)),
        },
        RestoreCandidate {
            window: other,
            fingerprint: &other_fingerprint,
            location: Some((space, workspace)),
        },
    ];

    let decision = choose_match(live, space, &live_fingerprint, None, &candidates).unwrap();

    assert_eq!(decision.selected, other);
    assert!(decision.exact_identity);
    assert!(decision.duplicate_identities.is_empty());
}

#[test]
fn reused_direct_window_identity_cannot_cross_known_application_identity() {
    use super::matcher::{RestoreCandidate, choose_match};

    let live = WindowId::new(42, 7);
    let compatible = WindowId::new(41, 6);
    let space = SpaceId::new(502);
    let workspace = crate::workspaces::VirtualWorkspaceId::default();
    let wrong_app = WindowFingerprint {
        window_server_id: Some(10),
        title: Some("Shared title".into()),
        width: 600.0,
        height: 800.0,
        app_id: Some("com.example.old".into()),
    };
    let right_app = WindowFingerprint {
        window_server_id: Some(20),
        title: Some("Shared title".into()),
        width: 600.0,
        height: 800.0,
        app_id: Some("com.example.current".into()),
    };
    let candidates = [
        RestoreCandidate {
            window: live,
            fingerprint: &wrong_app,
            location: Some((space, workspace)),
        },
        RestoreCandidate {
            window: compatible,
            fingerprint: &right_app,
            location: Some((space, workspace)),
        },
    ];

    let decision = choose_match(live, space, &right_app, None, &candidates).unwrap();

    assert_eq!(decision.selected, compatible);
    assert!(decision.exact_identity);
}

#[test]
fn restored_window_server_id_cannot_cross_known_application_identity() {
    let mut window_store = WindowStore::default();
    let mut engine = test_engine();
    let mut memory = DisplayMemory::default();
    let space = SpaceId::new(88);
    let titled_match = WindowId::new(1, 1);
    let id_match = WindowId::new(1, 2);
    let live = WindowId::new(99, 1);
    let _ = engine.handle_event(
        &mut window_store,
        &mut memory,
        LayoutEvent::SpaceExposed(space, CGSize::new(1200.0, 800.0)),
    );
    let _ = engine.handle_event(
        &mut window_store,
        &mut memory,
        LayoutEvent::WindowAdded(space, titled_match),
    );
    let _ = engine.handle_event(
        &mut window_store,
        &mut memory,
        LayoutEvent::WindowAdded(space, id_match),
    );
    engine.persistence.windows.insert(
        titled_match,
        WindowFingerprint {
            window_server_id: Some(10),
            title: Some("Current title".into()),
            width: 800.0,
            height: 600.0,
            app_id: Some("com.example.one".into()),
        },
    );
    engine.persistence.windows.insert(
        id_match,
        WindowFingerprint {
            window_server_id: Some(20),
            title: Some("Old title".into()),
            width: 500.0,
            height: 400.0,
            app_id: Some("com.example.two".into()),
        },
    );
    engine.persistence.pending_windows.extend([titled_match, id_match]);

    engine.reconcile_restored_window(
        &mut window_store,
        &mut memory,
        space,
        live,
        &WindowFingerprint {
            window_server_id: Some(20),
            title: Some("Current title".into()),
            width: 800.0,
            height: 600.0,
            app_id: Some("com.example.one".into()),
        },
    );

    let workspace = engine.active_workspace(space).unwrap();
    let layout = engine.workspace_layouts.active(space, workspace).unwrap();
    let windows = engine.workspace_tree(workspace).visible_windows_in_layout(layout);
    assert!(!windows.contains(&titled_match));
    assert!(windows.contains(&live));
    assert!(windows.contains(&id_match));
    assert_eq!(window_store.workspace_for_window(space, live), Some(workspace));
}

#[test]
fn fuzzy_match_requires_window_specific_evidence() {
    use super::matcher::{RestoreCandidate, choose_match};

    let saved = WindowId::new(42, 7);
    let live = WindowId::new(99, 1);
    let space = SpaceId::new(502);
    let saved_fingerprint = WindowFingerprint {
        window_server_id: None,
        title: Some("Music".into()),
        width: 500.0,
        height: 500.0,
        app_id: Some("com.example.app".into()),
    };
    let unrelated_live = WindowFingerprint {
        window_server_id: None,
        title: Some("Preferences".into()),
        width: 900.0,
        height: 700.0,
        app_id: Some("com.example.app".into()),
    };
    let candidate = [RestoreCandidate {
        window: saved,
        fingerprint: &saved_fingerprint,
        location: Some((space, crate::workspaces::VirtualWorkspaceId::default())),
    }];

    assert!(choose_match(live, space, &unrelated_live, None, &candidate).is_none());

    let title_only_match = WindowFingerprint {
        title: Some("Music".into()),
        ..unrelated_live
    };
    assert!(choose_match(live, space, &title_only_match, None, &candidate).is_none());

    let title_and_size_match = WindowFingerprint {
        width: 500.0,
        height: 500.0,
        ..title_only_match
    };
    assert_eq!(
        choose_match(live, space, &title_and_size_match, None, &candidate)
            .map(|decision| decision.selected),
        Some(saved)
    );

    let unknown_app_saved = WindowFingerprint {
        app_id: None,
        ..saved_fingerprint
    };
    let common_title_different_size = WindowFingerprint {
        window_server_id: None,
        title: Some("Music".into()),
        width: 1200.0,
        height: 900.0,
        app_id: Some("com.example.other".into()),
    };
    let unknown_candidate = [RestoreCandidate {
        window: saved,
        fingerprint: &unknown_app_saved,
        location: Some((space, crate::workspaces::VirtualWorkspaceId::default())),
    }];
    assert!(
        choose_match(
            live,
            space,
            &common_title_different_size,
            None,
            &unknown_candidate
        )
        .is_none()
    );
}

#[test]
fn rejected_fuzzy_candidate_is_removed_when_discovery_finishes() {
    let mut engine = test_engine();
    let mut memory = DisplayMemory::default();
    let mut window_store = WindowStore::default();
    let space = SpaceId::new(503);
    let ghost = WindowId::new(42, 7);
    let live = WindowId::new(99, 8);
    let _ = engine.handle_event(
        &mut window_store,
        &mut memory,
        LayoutEvent::SpaceExposed(space, CGSize::new(1200.0, 800.0)),
    );
    for window in [ghost, live] {
        let _ = engine.handle_event(
            &mut window_store,
            &mut memory,
            LayoutEvent::WindowAdded(space, window),
        );
    }
    engine.persistence.windows.insert(
        ghost,
        WindowFingerprint {
            window_server_id: None,
            title: Some("Music".into()),
            width: 500.0,
            height: 500.0,
            app_id: Some("com.example.app".into()),
        },
    );
    engine.persistence.pending_windows.insert(ghost);

    let outcome = engine.reconcile_restored_window(
        &mut window_store,
        &mut memory,
        space,
        live,
        &WindowFingerprint {
            window_server_id: None,
            title: Some("Preferences".into()),
            width: 900.0,
            height: 700.0,
            app_id: Some("com.example.app".into()),
        },
    );
    assert!(!outcome.matched);
    assert!(engine.persistence.pending_windows.contains(&ghost));

    let _ = engine.handle_event(
        &mut window_store,
        &mut memory,
        LayoutEvent::WindowDiscoveryCompleted(
            live.pid,
            Some("com.example.app".into()),
            vec![space],
        ),
    );
    let workspace = engine.active_workspace(space).unwrap();
    let layout = engine.workspace_layouts.active(space, workspace).unwrap();

    assert!(!engine.workspace_tree(workspace).contains_window(layout, ghost));
    assert!(!engine.persistence.windows.contains_key(&ghost));
    assert!(engine.workspace_tree(workspace).contains_window(layout, live));
}

#[test]
fn restore_fallback_requires_title_and_size_within_known_app() {
    let mut window_store = WindowStore::default();
    let mut engine = test_engine();
    let mut memory = DisplayMemory::default();
    let space = SpaceId::new(89);
    let title_match = WindowId::new(1, 1);
    let size_and_app_match = WindowId::new(1, 2);
    let live = WindowId::new(99, 1);
    let _ = engine.handle_event(
        &mut window_store,
        &mut memory,
        LayoutEvent::SpaceExposed(space, CGSize::new(1200.0, 800.0)),
    );
    let _ = engine.handle_event(
        &mut window_store,
        &mut memory,
        LayoutEvent::WindowAdded(space, title_match),
    );
    let _ = engine.handle_event(
        &mut window_store,
        &mut memory,
        LayoutEvent::WindowAdded(space, size_and_app_match),
    );
    engine.persistence.windows.insert(
        title_match,
        WindowFingerprint {
            window_server_id: None,
            title: Some("Project".into()),
            width: 400.0,
            height: 300.0,
            app_id: Some("com.example.other".into()),
        },
    );
    engine.persistence.windows.insert(
        size_and_app_match,
        WindowFingerprint {
            window_server_id: None,
            title: Some("Other".into()),
            width: 800.0,
            height: 600.0,
            app_id: Some("com.example.editor".into()),
        },
    );
    engine.persistence.pending_windows.extend([title_match, size_and_app_match]);

    engine.reconcile_restored_window(
        &mut window_store,
        &mut memory,
        space,
        live,
        &WindowFingerprint {
            window_server_id: None,
            title: Some("Project".into()),
            width: 800.0,
            height: 600.0,
            app_id: Some("com.example.editor".into()),
        },
    );

    let workspace = engine.active_workspace(space).unwrap();
    let layout = engine.workspace_layouts.active(space, workspace).unwrap();
    let windows = engine.workspace_tree(workspace).visible_windows_in_layout(layout);
    assert!(windows.contains(&title_match));
    assert!(!windows.contains(&live));
    assert!(windows.contains(&size_and_app_match));
}

#[test]
fn duplicate_window_server_fingerprints_choose_live_assignment_and_are_healed() {
    let mut window_store = WindowStore::default();
    let mut engine = test_engine();
    let mut memory = DisplayMemory::default();
    let space = SpaceId::new(91);
    let stale = WindowId::new(1, 1);
    let preferred = WindowId::new(1, 2);
    let live = WindowId::new(99, 1);
    let _ = engine.handle_event(
        &mut window_store,
        &mut memory,
        LayoutEvent::SpaceExposed(space, CGSize::new(1200.0, 800.0)),
    );
    let workspaces = engine.virtual_workspace_manager.list_workspaces(space);
    let stale_workspace = workspaces[0].0;
    let preferred_workspace = workspaces[1].0;
    let stale_layout = engine.workspace_layouts.active(space, stale_workspace).unwrap();
    let preferred_layout = engine.workspace_layouts.active(space, preferred_workspace).unwrap();
    engine
        .workspace_tree_mut(stale_workspace)
        .add_window_after_selection(stale_layout, stale);
    engine
        .workspace_tree_mut(preferred_workspace)
        .add_window_after_selection(preferred_layout, preferred);
    assert!(engine.virtual_workspace_manager.assign_window_to_workspace(
        &mut window_store,
        space,
        live,
        preferred_workspace,
    ));
    let fingerprint = |title: &str| WindowFingerprint {
        window_server_id: Some(42),
        title: Some(title.into()),
        width: 800.0,
        height: 600.0,
        app_id: Some("com.example.editor".into()),
    };
    engine.persistence.windows.insert(stale, fingerprint("stale"));
    engine.persistence.windows.insert(preferred, fingerprint("preferred"));
    engine.persistence.pending_windows.extend([stale, preferred]);

    engine.reconcile_restored_window(
        &mut window_store,
        &mut memory,
        space,
        live,
        &fingerprint("live"),
    );

    assert_eq!(
        window_store.workspace_for_window(space, live),
        Some(preferred_workspace),
    );
    assert!(!engine.persistence.windows.contains_key(&stale));
    assert!(!engine.workspace_tree(stale_workspace).contains_window(stale_layout, stale));
    assert!(
        engine
            .workspace_tree(preferred_workspace)
            .contains_window(preferred_layout, live)
    );
}

#[test]
fn duplicate_restored_identity_prefers_live_workspace_assignment() {
    let mut window_store = WindowStore::default();
    let mut engine = test_engine();
    let mut memory = DisplayMemory::default();
    let space = SpaceId::new(90);
    let live = WindowId::new(99, 7);

    let _ = engine.handle_event(
        &mut window_store,
        &mut memory,
        LayoutEvent::SpaceExposed(space, CGSize::new(1200.0, 800.0)),
    );
    let workspaces = engine.virtual_workspace_manager.list_workspaces(space);
    let stale_workspace = workspaces[0].0;
    let preferred_workspace = workspaces[1].0;
    let stale_layout = engine.workspace_layouts.active(space, stale_workspace).unwrap();
    let preferred_layout = engine.workspace_layouts.active(space, preferred_workspace).unwrap();
    engine
        .workspace_tree_mut(stale_workspace)
        .add_window_after_selection(stale_layout, live);
    engine
        .workspace_tree_mut(preferred_workspace)
        .add_window_after_selection(preferred_layout, live);
    assert!(engine.virtual_workspace_manager.assign_window_to_workspace(
        &mut window_store,
        space,
        live,
        preferred_workspace,
    ));

    let fingerprint = WindowFingerprint {
        window_server_id: Some(700),
        title: Some("Editor".into()),
        width: 800.0,
        height: 600.0,
        app_id: Some("dev.zed.Zed".into()),
    };
    engine.persistence.windows.insert(live, fingerprint.clone());
    engine.persistence.pending_windows.insert(live);

    engine.reconcile_restored_window(&mut window_store, &mut memory, space, live, &fingerprint);

    assert_eq!(
        window_store.workspace_for_window(space, live),
        Some(preferred_workspace),
    );
    assert!(!engine.workspace_tree(stale_workspace).contains_window(stale_layout, live));
    assert!(
        engine
            .workspace_tree(preferred_workspace)
            .contains_window(preferred_layout, live)
    );
}

#[test]
fn workspace_restore_preserves_live_window_when_saved_process_local_id_is_reused() {
    let space = SpaceId::new(133);
    let size = CGSize::new(1200.0, 800.0);
    let frame = objc2_core_foundation::CGRect::new(
        objc2_core_foundation::CGPoint::new(10.0, 20.0),
        CGSize::new(700.0, 500.0),
    );
    let reused = WindowId::new(76, 1);

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
        .add_window_after_selection(source_layout, reused);
    snapshot.persistence.windows.insert(
        reused,
        WindowFingerprint {
            window_server_id: Some(7600),
            title: Some("Old window".into()),
            width: 700.0,
            height: 500.0,
            app_id: Some("com.example.old".into()),
        },
    );
    let path = std::env::temp_dir().join(format!(
        "rini-reused-id-workspace-restore-test-{}-{}.ron",
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
    window_store.insert_window(
        reused,
        WindowState {
            info: WindowInfo {
                is_standard: true,
                is_root: true,
                is_minimized: false,
                is_resizable: true,
                min_size: None,
                max_size: None,
                title: "Current window".into(),
                frame,
                sys_id: Some(WindowServerId::new(7601)),
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
        space,
        reused,
        target_workspace,
    ));
    engine.add_window_to_layout(&mut window_store, &mut engine_memory, space, reused);

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
    assert!(engine.workspace_tree(target_workspace).contains_window(target_layout, reused));
    assert_eq!(
        window_store.workspace_for_window(space, reused),
        Some(target_workspace)
    );
    assert_eq!(engine.persistence.windows[&reused].window_server_id, Some(7601));
    assert_eq!(
        engine.persistence.windows[&reused].app_id.as_deref(),
        Some("com.example.current")
    );
}
