//! Where an application's windows go when it comes back, keyed so it survives the process.

use crate::workspaces::domain::display_memory::DisplayMemory;
use objc2_core_foundation::{CGPoint, CGRect, CGSize};

use super::*;
use crate::windows::domain::info::WindowInfo;
use crate::windows::domain::state::WindowState;
use crate::workspaces::LayoutEvent;
use rini_core::ids::WindowServerId;

/// The whole point of the durable key is that it outlives the process, so it has to be in the file. A
/// window's own records are keyed by pid and are expected to be useless on the next boot; these are not.
#[test]
fn launch_memory_survives_a_save_and_load() {
    use crate::workspaces::domain::display_affinity::ColumnWidth;
    use crate::workspaces::domain::launch_memory::{Slot, topology_key};

    let engine = test_engine();
    let _memory = DisplayMemory::default();

    let mut memory = DisplayMemory::default();
    let docked = topology_key(&["built-in".into(), "external".into()]);
    let alone = topology_key(&["built-in".into()]);
    memory.launch.remember(
        "com.mitchellh.ghostty",
        &docked,
        vec![
            Slot {
                title: Some("~/projects/rini".into()),
                display_uuid: "external".into(),
                workspace_index: 2,
                width: Some(ColumnWidth::Offset(0.25)),
            }
            .into(),
        ],
    );
    memory.launch.remember(
        "com.mitchellh.ghostty",
        &alone,
        vec![
            Slot {
                title: Some("~/projects/rini".into()),
                display_uuid: "built-in".into(),
                workspace_index: 0,
                width: Some(ColumnWidth::FullWidth),
            }
            .into(),
        ],
    );

    let path =
        std::env::temp_dir().join(format!("rini-launch-memory-test-{}.ron", std::process::id()));
    engine.save(&mut memory, path.clone()).unwrap();
    let _loaded = LayoutEngine::load(path.clone()).unwrap();
    let _ = std::fs::remove_file(path);

    let docked_slots = memory.launch.slots("com.mitchellh.ghostty", &docked);
    assert_eq!(docked_slots.len(), 1);
    assert_eq!(docked_slots[0].display_uuid, "external");
    assert_eq!(docked_slots[0].workspace_index, 2);
    assert_eq!(docked_slots[0].width, Some(ColumnWidth::Offset(0.25)));

    let alone_slots = memory.launch.slots("com.mitchellh.ghostty", &alone);
    assert_eq!(alone_slots[0].display_uuid, "built-in");
    assert_eq!(alone_slots[0].width, Some(ColumnWidth::FullWidth));
}

/// The whole feature, end to end: a window is placed, moved, sized, and remembered; then the application
/// relaunches with a new pid and the window has to land back where it was rather than in whatever
/// workspace happens to be active at the time.
#[test]
fn a_relaunched_window_returns_to_its_remembered_workspace_and_width() {
    use crate::workspaces::domain::display_affinity::ColumnWidth;

    const DISPLAY: &str = "37D8832A-2D66-02CA-B9F7-8F30A301B230";
    let mut engine = test_engine();
    let mut memory = DisplayMemory::default();
    let mut window_store = WindowStore::default();
    let space = SpaceId::new(11);
    let before_quit = WindowId::new(500, 1);
    let after_relaunch = WindowId::new(900, 7);

    let _ = engine.handle_event(
        &mut window_store,
        &mut memory,
        LayoutEvent::SpaceExposed(space, CGSize::new(1728.0, 1085.0)),
    );
    engine.update_space_display(&mut memory, space, Some(DISPLAY.to_owned()));
    engine.set_connected_displays(vec![DISPLAY.to_owned()]);

    let insert = |store: &mut WindowStore, window: WindowId| {
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
                    title: "~/projects/rini".into(),
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
    };

    // The window as it was before the application quit: moved off the active workspace and made full
    // width, which are the two things that used to be forgotten.
    insert(&mut window_store, before_quit);
    let _ = engine.handle_event(
        &mut window_store,
        &mut memory,
        LayoutEvent::WindowAdded(space, before_quit),
    );
    let workspaces: Vec<_> = engine
        .virtual_workspace_manager
        .list_workspaces(space)
        .into_iter()
        .map(|(id, _)| id)
        .collect();
    let elsewhere = workspaces[2];
    assert!(engine.virtual_workspace_manager.assign_window_to_workspace(
        &mut window_store,
        space,
        before_quit,
        elsewhere,
    ));
    memory.affinity.set_window_width(DISPLAY, before_quit, ColumnWidth::FullWidth);

    engine.remember_launch_slots(&window_store, &mut memory, &[DISPLAY.to_owned()]);

    // The application quits and comes back with a different pid and window server id.
    let _ = engine.handle_event(
        &mut window_store,
        &mut memory,
        LayoutEvent::WindowRemoved(before_quit),
    );

    insert(&mut window_store, after_relaunch);
    let _ = engine.handle_event(
        &mut window_store,
        &mut memory,
        LayoutEvent::WindowAdded(space, after_relaunch),
    );

    assert_eq!(
        engine
            .virtual_workspace_manager
            .workspace_for_window_any(&window_store, after_relaunch),
        Some(elsewhere),
        "back in the workspace it was in, not the active one"
    );
    assert_eq!(
        memory.affinity.window_home(after_relaunch),
        Some(DISPLAY),
        "and on the display it belonged to"
    );
    assert_eq!(
        memory.affinity.window_width(DISPLAY, after_relaunch),
        Some(ColumnWidth::FullWidth),
        "at the width it had there"
    );
    // The record is not the thing that decides how wide the column is. Reported live: the window opened
    // full sized and was instantly resized to half, because the width reached the affinity record and
    // never reached the layout.
    let layout = engine
        .workspace_layouts
        .active(space, elsewhere)
        .expect("the workspace it landed in has a layout");
    assert!(
        engine.workspace_tree(elsewhere).is_window_full_width(layout, after_relaunch),
        "and the LAYOUT is what has to know it, not just the record"
    );
}

/// The measured failure. The projection asked which space a window was in with a lookup that only
/// consults each space's ACTIVE workspace, so a window parked in a workspace nobody was looking at
/// produced no slot — and an application whose windows were all parked had its entry wiped on the next
/// save. Seen live: TextEdit moved to another workspace, then gone from the file entirely.
#[test]
fn a_window_in_a_workspace_nobody_is_looking_at_is_still_remembered() {
    const DISPLAY: &str = "37D8832A-2D66-02CA-B9F7-8F30A301B230";
    let mut engine = test_engine();
    let mut memory = DisplayMemory::default();
    let mut window_store = WindowStore::default();
    let space = SpaceId::new(21);
    let window = WindowId::new(600, 3);

    let _ = engine.handle_event(
        &mut window_store,
        &mut memory,
        LayoutEvent::SpaceExposed(space, CGSize::new(1728.0, 1085.0)),
    );
    engine.update_space_display(&mut memory, space, Some(DISPLAY.to_owned()));
    engine.set_connected_displays(vec![DISPLAY.to_owned()]);

    let frame = CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(800.0, 600.0));
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
                title: "Untitled".into(),
                frame,
                sys_id: Some(WindowServerId::new(8571)),
                bundle_id: Some("com.apple.TextEdit".into()),
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
    let _ = engine.handle_event(
        &mut window_store,
        &mut memory,
        LayoutEvent::WindowAdded(space, window),
    );

    // Park it in a workspace that is not the active one, through the command the user runs, so its tree
    // membership really moves. Reassigning it in the store alone leaves it in the active workspace's tree,
    // where the old lookup could still find it and the test would prove nothing.
    let parked_index = 2usize;
    let _ = engine.handle_virtual_workspace_command(
        &mut window_store,
        &mut memory,
        space,
        &crate::workspaces::LayoutCommand::MoveWindowToWorkspace {
            workspace: rini_ipc::protocol::WorkspaceSelector::Index(parked_index),
            follow: false,
            window_id: Some(window.idx.get()),
        },
    );
    let workspaces: Vec<_> = engine
        .virtual_workspace_manager
        .list_workspaces(space)
        .into_iter()
        .map(|(id, _)| id)
        .collect();
    let active = engine.virtual_workspace_manager.active_workspace(space).unwrap();
    assert_ne!(
        workspaces[parked_index], active,
        "parked away from the active workspace"
    );

    engine.remember_launch_slots(&window_store, &mut memory, &[DISPLAY.to_owned()]);

    let topology = crate::workspaces::domain::launch_memory::topology_key(&[DISPLAY.to_owned()]);
    let slots = memory.launch.slots("com.apple.TextEdit", &topology);
    assert_eq!(slots.len(), 1, "a parked window still has a slot");
    assert_eq!(
        slots[0].workspace_index, parked_index,
        "and it names the workspace it is parked in"
    );
}

/// A display home is written once, on first sighting, and only if the space's display was known by then.
/// A window that appeared before that mapping existed has none, forever — measured live on a window that
/// rini tracked and had assigned to a workspace. Requiring the home for the projection meant such a window
/// was never remembered at all, so the space's own display stands in for it.
#[test]
fn a_window_with_no_recorded_home_is_remembered_against_its_spaces_display() {
    const DISPLAY: &str = "37D8832A-2D66-02CA-B9F7-8F30A301B230";
    let mut engine = test_engine();
    let mut memory = DisplayMemory::default();
    let mut window_store = WindowStore::default();
    let space = SpaceId::new(31);
    let window = WindowId::new(700, 4);

    let _ = engine.handle_event(
        &mut window_store,
        &mut memory,
        LayoutEvent::SpaceExposed(space, CGSize::new(1728.0, 1085.0)),
    );
    engine.update_space_display(&mut memory, space, Some(DISPLAY.to_owned()));
    engine.set_connected_displays(vec![DISPLAY.to_owned()]);

    let frame = CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(800.0, 600.0));
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
                title: "Untitled".into(),
                frame,
                sys_id: Some(WindowServerId::new(8610)),
                bundle_id: Some("com.apple.TextEdit".into()),
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
    let _ = engine.handle_event(
        &mut window_store,
        &mut memory,
        LayoutEvent::WindowAdded(space, window),
    );

    // Exactly the state seen live: tracked, assigned, no home.
    memory.affinity.forget_window(window);
    assert_eq!(memory.affinity.window_home(window), None);

    engine.remember_launch_slots(&window_store, &mut memory, &[DISPLAY.to_owned()]);

    let topology = crate::workspaces::domain::launch_memory::topology_key(&[DISPLAY.to_owned()]);
    let slots = memory.launch.slots("com.apple.TextEdit", &topology);
    assert_eq!(slots.len(), 1, "remembered even with no home of its own");
    assert_eq!(
        slots[0].display_uuid, DISPLAY,
        "against the display its space is on"
    );
}

/// The first sighting of a launching application's window happens while the app rules are applied, and the
/// window is NOT in the window store yet at that point. Reading its bundle id and title from the store
/// there is why the whole feature silently did nothing: the lookup returned early, before it had even
/// consulted the memory. Identity has to come from the caller, which has it.
///
/// The window is deliberately inserted here WITHOUT a bundle id, so a lookup that consults the store
/// cannot pass.
#[test]
fn a_launching_window_is_placed_from_the_identity_the_rules_were_given() {
    use crate::workspaces::domain::display_affinity::ColumnWidth;
    use crate::workspaces::domain::launch_memory::{Slot, topology_key};

    const DISPLAY: &str = "37D8832A-2D66-02CA-B9F7-8F30A301B230";
    let mut engine = test_engine();
    let mut memory = DisplayMemory::default();
    let mut window_store = WindowStore::default();
    let space = SpaceId::new(41);
    let window = WindowId::new(800, 5);

    let _ = engine.handle_event(
        &mut window_store,
        &mut memory,
        LayoutEvent::SpaceExposed(space, CGSize::new(1728.0, 1085.0)),
    );
    engine.update_space_display(&mut memory, space, Some(DISPLAY.to_owned()));
    engine.set_connected_displays(vec![DISPLAY.to_owned()]);

    let frame = CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(800.0, 600.0));
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
                title: String::new(),
                frame,
                sys_id: Some(WindowServerId::new(8769)),
                bundle_id: None,
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

    let topology = topology_key(&[DISPLAY.to_owned()]);
    memory.launch.remember(
        "com.apple.TextEdit",
        &topology,
        vec![
            Slot {
                title: Some("Untitled".into()),
                display_uuid: DISPLAY.to_owned(),
                workspace_index: 1,
                width: Some(ColumnWidth::FullWidth),
            }
            .into(),
        ],
    );

    let result = engine
        .assign_window_with_app_info(
            &mut window_store,
            &mut memory,
            window,
            space,
            Some("com.apple.TextEdit"),
            Some("TextEdit"),
            Some("Untitled"),
            None,
            None,
            false,
        )
        .expect("assignment should succeed");

    let workspaces: Vec<_> = engine
        .virtual_workspace_manager
        .list_workspaces(space)
        .into_iter()
        .map(|(id, _)| id)
        .collect();
    match result {
        crate::workspaces::AppRuleResult::Managed(effects) => {
            assert_eq!(
                effects.workspace_id, workspaces[1],
                "placed in the remembered workspace"
            );
        }
        other => panic!("expected a managed placement, got {other:?}"),
    }
    assert_eq!(
        memory.affinity.window_width(DISPLAY, window),
        Some(ColumnWidth::FullWidth),
        "and given the width it had there"
    );
}

/// The width has to come from the window's own layout, not from the affinity map. That map is only written
/// by explicit width COMMANDS, so a window whose width came from the layout itself never appeared in it —
/// measured live as `window_width:{}` in the file with every slot recording no width, which is why a
/// relaunched window came back at the default no matter what it had been.
#[test]
fn a_width_the_layout_gave_a_window_is_remembered_without_a_width_command() {
    use crate::workspaces::domain::display_affinity::ColumnWidth;
    use crate::workspaces::domain::launch_memory::topology_key;

    const DISPLAY: &str = "37D8832A-2D66-02CA-B9F7-8F30A301B230";
    let mut engine = test_engine();
    let mut memory = DisplayMemory::default();
    let mut window_store = WindowStore::default();
    let space = SpaceId::new(51);
    let window = WindowId::new(900, 2);

    let _ = engine.handle_event(
        &mut window_store,
        &mut memory,
        LayoutEvent::SpaceExposed(space, CGSize::new(1728.0, 1085.0)),
    );
    engine.update_space_display(&mut memory, space, Some(DISPLAY.to_owned()));
    engine.set_connected_displays(vec![DISPLAY.to_owned()]);

    let frame = CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(800.0, 600.0));
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
                title: "Zen Browser".into(),
                frame,
                sys_id: Some(WindowServerId::new(9002)),
                bundle_id: Some("app.zen-browser.zen".into()),
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
    let _ = engine.handle_event(
        &mut window_store,
        &mut memory,
        LayoutEvent::WindowAdded(space, window),
    );

    // Full width in the LAYOUT with nothing in the affinity map, which is the state the file was actually
    // in: `window_width:{}` and every slot recording no width at all.
    let ws_id = engine.virtual_workspace_manager.active_workspace(space).unwrap();
    let layout = engine.workspace_layouts.active(space, ws_id).unwrap();
    engine.workspace_tree_mut(ws_id).set_window_full_width(layout, window, true);
    assert!(
        engine.workspace_tree(ws_id).is_window_full_width(layout, window),
        "setup"
    );
    assert_eq!(memory.affinity.window_width(DISPLAY, window), None, "setup");

    engine.remember_launch_slots(&window_store, &mut memory, &[DISPLAY.to_owned()]);

    let topology = topology_key(&[DISPLAY.to_owned()]);
    let slots = memory.launch.slots("app.zen-browser.zen", &topology);
    assert_eq!(slots.len(), 1);
    assert_eq!(
        slots[0].width,
        Some(ColumnWidth::FullWidth),
        "the width came from the layout, with nothing in the affinity map"
    );
}

/// The bug that came back three times. ctrl-F, quit rini, start it again: the window relaunched at the
/// default half width, and the next autosave then overwrote `FullWidth` with nothing, so the record was
/// gone for good and ctrl-F had to be pressed again.
///
/// The projection could not read the width here — this fixture withholds the topology, as a startup
/// where windows are discovered before the first authoritative space snapshot does — and "could not
/// read it" was indistinguishable from "it has no width". See `ProjectedWidth`.
#[test]
fn a_save_that_cannot_read_the_width_does_not_erase_a_remembered_full_width() {
    use crate::workspaces::domain::display_affinity::ColumnWidth;
    use crate::workspaces::engine::LayoutCommand;

    const DISPLAY: &str = "37D8832A-2D66-02CA-B9F7-8F30A301B230";
    const APP: &str = "com.mitchellh.ghostty";
    let space = SpaceId::new(11);

    let insert = |store: &mut WindowStore, window: WindowId| {
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
                    title: "~/projects/rini".into(),
                    frame,
                    sys_id: Some(WindowServerId::new(window.idx.get())),
                    bundle_id: Some(APP.into()),
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
    };

    let mut engine = test_engine();
    let _memory = DisplayMemory::default();

    let mut memory = DisplayMemory::default();
    let mut store = WindowStore::default();
    let before_quit = WindowId::new(500, 1);
    let _ = engine.handle_event(
        &mut store,
        &mut memory,
        LayoutEvent::SpaceExposed(space, CGSize::new(1728.0, 1085.0)),
    );
    engine.update_space_display(&mut memory, space, Some(DISPLAY.to_owned()));
    engine.set_connected_displays(vec![DISPLAY.to_owned()]);
    insert(&mut store, before_quit);
    let _ = engine.handle_event(
        &mut store,
        &mut memory,
        LayoutEvent::WindowAdded(space, before_quit),
    );

    engine.focused_window = Some(before_quit);
    let _ = engine.handle_command(
        &mut store,
        &mut memory,
        Some(space),
        &[space],
        &HashMap::default(),
        LayoutCommand::ToggleFullscreenWithinGaps,
    );
    engine.remember_launch_slots(&store, &mut memory, &[DISPLAY.to_owned()]);

    let topology = crate::workspaces::domain::launch_memory::topology_key(&[DISPLAY.to_owned()]);
    assert_eq!(
        memory.launch.slots(APP, &topology)[0].width,
        Some(ColumnWidth::FullWidth),
        "ctrl-F is what gets remembered"
    );

    // rini restarts, and the window is discovered before the topology is known.
    let path = std::env::temp_dir().join(format!("full_width_{}.ron", std::process::id()));
    engine.save(&mut memory, path.clone()).unwrap();
    let mut reloaded = LayoutEngine::load(path.clone()).unwrap();
    let _ = std::fs::remove_file(&path);

    let mut store = WindowStore::default();
    let after_relaunch = WindowId::new(900, 7);
    let _ = reloaded.handle_event(
        &mut store,
        &mut memory,
        LayoutEvent::SpaceExposed(space, CGSize::new(1728.0, 1085.0)),
    );
    reloaded.update_space_display(&mut memory, space, Some(DISPLAY.to_owned()));
    reloaded.set_connected_displays(vec![DISPLAY.to_owned()]);
    insert(&mut store, after_relaunch);
    let _ = reloaded.handle_event(
        &mut store,
        &mut memory,
        LayoutEvent::WindowAdded(space, after_relaunch),
    );

    // The autosave fires while the width still cannot be read. This is the step that used to
    // destroy the record, and after it the window can never come back full width again.
    reloaded.remember_launch_slots(&store, &mut memory, &[DISPLAY.to_owned()]);
    assert_eq!(
        memory.launch.slots(APP, &topology)[0].width,
        Some(ColumnWidth::FullWidth),
        "a save that could not read the width must leave the remembered one standing"
    );
}
