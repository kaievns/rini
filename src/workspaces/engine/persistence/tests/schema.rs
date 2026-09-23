//! What a saved file may look like, and what is refused at the load boundary.

use crate::workspaces::domain::display_memory::DisplayMemory;
use objc2_core_foundation::CGSize;

use super::*;
use crate::workspaces::VirtualWorkspace;
use crate::workspaces::{LayoutEvent, ScrollingLayoutSystem};

#[test]
fn persisted_layout_schema_is_versioned_and_legacy_files_still_load() {
    let engine = test_engine();
    let memory = DisplayMemory::default();
    let serialized = engine.serialize_to_string(&memory);
    assert!(serialized.contains("\"schema_version\":5"), "{serialized}");

    let legacy = serialized.replacen("\"schema_version\":5,", "", 1);
    LayoutEngine::deserialize_from_str(&legacy).unwrap();

    let future = serialized.replacen("\"schema_version\":5", "\"schema_version\":6", 1);
    let error = match LayoutEngine::deserialize_from_str(&future) {
        Ok(_) => panic!("future schema version should be rejected"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("newer than supported"));
}

/// A v2 file on disk carried two separate display maps. Both must survive the upgrade,
/// or every display looks unseen after it and a replug has nothing to match against.
#[test]
fn schema_v2_display_maps_are_folded_into_the_affinity_registry() {
    let engine = test_engine();
    let empty = DisplayMemory::default();
    let v2 = engine
        .serialize_to_string(&empty)
        .replacen("\"schema_version\":5", "\"schema_version\":2", 1)
        .replacen(
            "\"display_memory\":(affinity:(display_space:{},window_home:{},display_strip:{},\
             window_width:{}),launch:(apps:{}))",
            "\"space_display_map\":{(7):Some(\"external-uuid\")},\
             \"display_last_space\":{\"builtin-uuid\":(1)}",
            1,
        );
    assert!(v2.contains("space_display_map"), "{v2}");

    let loaded = LayoutEngine::deserialize_file(&v2).expect("v2 file must load");
    let memory = loaded.memory;

    assert_eq!(
        memory.affinity.space_for_display("external-uuid"),
        Some(SpaceId::new(7)),
        "the space -> display map must carry over"
    );
    assert_eq!(
        memory.affinity.space_for_display("builtin-uuid"),
        Some(SpaceId::new(1)),
        "the display -> space map must carry over"
    );
    let upgraded = loaded.layout.expect("the layout itself is fine");
    let rewritten = upgraded.serialize_to_string(&memory);
    assert!(
        !rewritten.contains("space_display_map"),
        "the upgraded file must be written in the new shape only"
    );
    assert!(
        rewritten.contains("\"display_memory\":(affinity:(display_space:{"),
        "and the memory must be nested under its own key: {rewritten}"
    );
    assert!(
        rewritten.contains("\"external-uuid\":(7)") && rewritten.contains("\"builtin-uuid\":(1)"),
        "with both displays inside it: {rewritten}"
    );
}

/// A schema-4 file kept its display memory at the top level. Schema 5 nests it, and an upgrade must
/// carry it across: a user's file is where the record of which monitor each window lives on actually
/// is, and losing it on upgrade is the same outcome as never having had it.
///
/// Verified against a real 19-window file from `~/.rini/layout.ron` at the time of the change; the
/// fixture here is built the same way so the check runs everywhere.
#[test]
fn a_schema_four_file_moves_its_display_memory_into_the_nested_record() {
    let mut engine = test_engine();
    let mut memory = DisplayMemory::default();
    let mut window_store = WindowStore::default();
    let space = SpaceId::new(4615);
    let window = WindowId::new(8, 1);
    let _ = engine.handle_event(
        &mut window_store,
        &mut memory,
        LayoutEvent::SpaceExposed(space, CGSize::new(1200.0, 800.0)),
    );
    engine.update_space_display(&mut memory, space, Some("studio-display".to_string()));
    memory.affinity.set_window_home(window, "studio-display");

    // The schema-4 shape: both records at the top level, no nested one.
    let v5 = engine.serialize_to_string(&memory);
    let nested_start = v5.find(r#","display_memory":("#).expect("the v5 shape");
    let nested_end = v5.find(r#","persisted_windows":"#).expect("the v5 shape");
    let affinity = v5[nested_start..nested_end]
        .trim_start_matches(r#","display_memory":(affinity:"#)
        .rsplit_once(",launch:")
        .expect("affinity then launch")
        .0
        .to_owned();
    let v4 = format!(
        "{}\"display_affinity\":{},\"launch_memory\":(apps:{{}}){}",
        &v5[..nested_start + 1],
        affinity,
        &v5[nested_end..]
    )
    .replacen(r#""schema_version":5"#, r#""schema_version":4"#, 1);
    assert!(
        v4.contains(r#""display_affinity":"#),
        "the fixture is in the old shape: {v4}"
    );
    assert!(
        !v4.contains(r#""display_memory":"#),
        "and only the old shape: {v4}"
    );

    let loaded = LayoutEngine::deserialize_file(&v4).expect("a schema-4 file must parse");
    assert!(loaded.layout.is_ok(), "and its layout must validate");
    assert_eq!(
        loaded.memory.affinity.window_home(window),
        Some("studio-display"),
        "the window's display comes across"
    );
    assert_eq!(
        loaded.memory.affinity.space_for_display("studio-display"),
        Some(space),
        "and so does the display's space"
    );

    let rewritten = loaded.layout.unwrap().serialize_to_string(&loaded.memory);
    assert!(rewritten.contains(r#""schema_version":5"#));
    assert!(
        rewritten.contains(r#""display_memory":("#),
        "written in the nested shape"
    );
    assert!(
        !rewritten.contains(r#""display_affinity":"#),
        "and not the old one"
    );
}

/// What happens to the layout file that exists on disk right now. Schema 4 dropped the
/// `LayoutSystemKind` wrapper, so a schema-3 file tags every layout `scrolling((...))` for an enum
/// that is gone. RON reports that as a shape mismatch deep inside the tree, which says nothing
/// useful, so the tag is looked for first and the answer is a sentence instead.
#[test]
fn a_layout_file_wrapped_in_the_old_layout_system_tag_is_refused_by_name() {
    let mut engine = test_engine();
    let mut memory = DisplayMemory::default();
    let mut store = WindowStore::default();
    let _ = engine.handle_event(
        &mut store,
        &mut memory,
        LayoutEvent::SpaceExposed(SpaceId::new(9), CGSize::new(1728.0, 1085.0)),
    );
    let current = engine.serialize_to_string(&memory);
    assert!(
        current.contains("layout_system"),
        "the fixture needs a workspace: {current}"
    );
    assert!(
        !current.contains("scrolling("),
        "schema 5 must not write the tag itself: {current}"
    );

    // A schema-3 file, as the previous build wrote them.
    let legacy = current.replacen("layout_system:(", "layout_system:scrolling((", 1);
    assert_ne!(
        legacy, current,
        "the fixture has to actually contain a layout_system"
    );

    let message = match LayoutEngine::deserialize_from_str(&legacy) {
        Ok(_) => panic!("a file carrying the old tag cannot be read"),
        Err(error) => error.to_string(),
    };
    assert!(
        message.contains("predates schema 5") && message.contains("laid out fresh"),
        "the error has to say what happened and what follows: {message}"
    );
}

/// The reason the display memory is its own record.
///
/// A layout written by a NEWER schema cannot be trusted, and rini refuses it. That refusal used to
/// take the machine's memory with it, because both lived in the same struct behind the same
/// validation: every window was then re-homed from scratch on the next display change and every
/// relaunched application landed in a default slot. Which monitor a window lives on is not a fact the
/// layout's version has any bearing on.
#[test]
fn a_layout_from_a_newer_schema_is_refused_without_forgetting_the_displays() {
    let mut engine = test_engine();
    let mut memory = DisplayMemory::default();
    let mut window_store = WindowStore::default();
    let space = SpaceId::new(31);
    let window = WindowId::new(4, 1);
    let _ = engine.handle_event(
        &mut window_store,
        &mut memory,
        LayoutEvent::SpaceExposed(space, CGSize::new(1200.0, 800.0)),
    );
    engine.update_space_display(&mut memory, space, Some("studio-display".to_string()));
    memory.affinity.set_window_home(window, "studio-display");

    let written = engine.serialize_to_string(&memory);
    let from_the_future = written.replacen(r#""schema_version":5"#, r#""schema_version":99"#, 1);
    assert_ne!(from_the_future, written);

    let loaded = LayoutEngine::deserialize_file(&from_the_future).expect("the bytes still parse");

    let error = match loaded.layout {
        Ok(_) => panic!("a newer schema is refused"),
        Err(error) => error.to_string(),
    };
    assert!(error.contains("newer than supported"), "{error}");
    assert_eq!(
        loaded.memory.affinity.window_home(window),
        Some("studio-display"),
        "the refusal is about the layout, not about which monitor the window lives on"
    );
    assert_eq!(
        loaded.memory.affinity.space_for_display("studio-display"),
        Some(space),
        "and the display still owns its space, so a replug has something to match"
    );
}

/// The same for a layout that fails validation rather than versioning.
#[test]
fn an_invalid_layout_is_refused_without_forgetting_the_displays() {
    let mut engine = test_engine();
    let mut memory = DisplayMemory::default();
    let mut window_store = WindowStore::default();
    let space = SpaceId::new(32);
    let _ = engine.handle_event(
        &mut window_store,
        &mut memory,
        LayoutEvent::SpaceExposed(space, CGSize::new(1200.0, 800.0)),
    );
    engine.update_space_display(&mut memory, space, Some("studio-display".to_string()));

    // A workspace order naming a workspace the file does not carry: invalid topology.
    let written = engine.serialize_to_string(&memory);
    let broken = written.replacen(
        "workspace_order:[(idx:1,version:1)",
        "workspace_order:[(idx:97,version:1)",
        1,
    );
    assert_ne!(
        broken, written,
        "the fixture has to actually carry a workspace order"
    );

    let loaded = LayoutEngine::deserialize_file(&broken).expect("the bytes still parse");

    assert!(loaded.layout.is_err(), "an invalid topology is refused");
    assert_eq!(
        loaded.memory.affinity.space_for_display("studio-display"),
        Some(space),
        "the display memory is read before the layout is validated"
    );
}

#[test]
fn malformed_active_layout_configuration_is_rejected_at_load_boundary() {
    let mut engine = test_engine();
    let mut memory = DisplayMemory::default();
    let mut window_store = WindowStore::default();
    let space = SpaceId::new(600);
    let _ = engine.handle_event(
        &mut window_store,
        &mut memory,
        LayoutEvent::SpaceExposed(space, CGSize::new(1200.0, 800.0)),
    );
    let mut serialized = engine.serialize_to_string(&memory);
    let active_size = serialized.find("active_size").unwrap_or_else(|| {
        panic!("serialized workspace must contain an active size: {serialized}")
    });
    let width = serialized[active_size..]
        .find("1200")
        .map(|offset| active_size + offset)
        .expect("serialized active size must contain the display width");
    serialized.replace_range(width..width + 4, "9999");

    let error = match LayoutEngine::deserialize_from_str(&serialized) {
        Ok(_) => panic!("invalid active layout configuration should be rejected"),
        Err(error) => error,
    };

    assert!(
        error.to_string().contains("no configuration for its active display size"),
        "{error}"
    );
}

#[test]
fn invalid_persisted_window_geometry_is_rejected() {
    let mut engine = test_engine();
    let memory = DisplayMemory::default();
    let window = WindowId::new(60, 1);
    engine.persistence.windows.insert(
        window,
        WindowFingerprint {
            window_server_id: Some(6001),
            title: Some("Invalid geometry".into()),
            width: -1.0,
            height: 500.0,
            app_id: Some("com.example.invalid".into()),
        },
    );

    let error = match LayoutEngine::deserialize_from_str(&engine.serialize_to_string(&memory)) {
        Ok(_) => panic!("invalid persisted window geometry should be rejected"),
        Err(error) => error,
    };

    assert!(error.to_string().contains("invalid persisted size"), "{error}");
}

#[test]
fn invalid_persisted_floating_frame_is_rejected() {
    let mut engine = test_engine();
    let mut memory = DisplayMemory::default();
    let mut window_store = WindowStore::default();
    let space = SpaceId::new(601);
    let window = WindowId::new(60, 2);
    let _ = engine.handle_event(
        &mut window_store,
        &mut memory,
        LayoutEvent::SpaceExposed(space, CGSize::new(1200.0, 800.0)),
    );
    let workspace = engine.active_workspace(space).unwrap();
    engine.floating_positions.store(
        space,
        workspace,
        window,
        objc2_core_foundation::CGRect::new(
            objc2_core_foundation::CGPoint::new(10.0, 20.0),
            CGSize::new(-1.0, 500.0),
        ),
    );

    let error = match LayoutEngine::deserialize_from_str(&engine.serialize_to_string(&memory)) {
        Ok(_) => panic!("invalid persisted floating frame should be rejected"),
        Err(error) => error,
    };

    assert!(error.to_string().contains("invalid floating frame"), "{error}");
}

/// A layout file written before this existed must still load, or an upgrade wipes the layout.
#[test]
fn a_file_without_launch_memory_still_loads() {
    let mut engine = test_engine();
    let mut memory = DisplayMemory::default();
    let mut window_store = WindowStore::default();
    let space = SpaceId::new(5);
    let _ = engine.handle_event(
        &mut window_store,
        &mut memory,
        LayoutEvent::SpaceExposed(space, CGSize::new(1200.0, 800.0)),
    );
    let path =
        std::env::temp_dir().join(format!("rini-launch-memory-legacy-{}.ron", std::process::id()));
    engine.save(&mut memory, path.clone()).unwrap();

    // Strip the record back out, as a file from before it existed would be.
    let written = std::fs::read_to_string(&path).unwrap();
    let stripped = written.replace(",launch:(apps:{})", "");
    assert_ne!(
        stripped, written,
        "the record is written, so this test is checking something"
    );
    std::fs::write(&path, stripped).unwrap();

    let loaded = LayoutEngine::load_file(&path).unwrap();
    let _ = std::fs::remove_file(path);
    assert!(
        loaded.layout.is_ok(),
        "a missing record is a default, not a refusal"
    );
    assert!(loaded.memory.launch.is_empty());
}

/// This used to compare `mem::discriminant`, asking whether the right layout SYSTEM came back out
/// of the file. With one system left that question has no content, so it asks the one that does:
/// whether the strip's own shape survives the trip.
#[test]
fn layout_system_round_trips_through_ron() {
    let mut system = VirtualWorkspace::create_layout_system(&LayoutSettings::default());
    let layout = system.create_layout();
    let (first, second) = (WindowId::new(400, 1), WindowId::new(400, 2));
    system.add_window_after_selection(layout, first);
    system.add_window_after_selection(layout, second);
    system.toggle_fold_of_selection(layout, crate::layout::Direction::Left);

    let serialized = ron::ser::to_string(&system).unwrap();
    let restored: ScrollingLayoutSystem = ron::from_str(&serialized).unwrap();

    assert!(restored.contains_layout(layout), "the layout id has to survive");
    assert_eq!(
        restored.all_windows_in_layout(layout),
        system.all_windows_in_layout(layout),
        "and so does the column the two windows were folded into"
    );
    assert_eq!(restored.selected_window(layout), Some(second));
}
