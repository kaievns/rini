//! What a saved file may look like, and what is refused at the load boundary.

use objc2_core_foundation::CGSize;

use super::*;
use crate::workspaces::domain::display_memory::DisplayMemory;
use crate::workspaces::{LayoutEvent, ScrollingLayoutSystem, VirtualWorkspace};

#[test]
fn persisted_layout_schema_is_versioned_and_any_other_version_is_refused() {
    let engine = test_engine();
    let memory = DisplayMemory::default();
    let serialized = engine.serialize_to_string(&memory);
    assert!(serialized.contains("\"schema_version\":6"), "{serialized}");
    LayoutEngine::deserialize_from_str(&serialized).unwrap();

    let unversioned = serialized.replacen("\"schema_version\":6,", "", 1);
    let error = match LayoutEngine::deserialize_from_str(&unversioned) {
        Ok(_) => panic!("a file naming no schema version should be rejected"),
        Err(error) => error,
    };
    assert!(
        error.to_string().contains("version 0 is older than 6"),
        "{error}"
    );

    let future = serialized.replacen("\"schema_version\":6", "\"schema_version\":7", 1);
    let error = match LayoutEngine::deserialize_from_str(&future) {
        Ok(_) => panic!("future schema version should be rejected"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("newer than supported"));
}

/// A file from before window records were kept per display arrangement is refused by name, and,
/// like any refused layout, without forgetting what its memory section holds in today's shape.
#[test]
fn a_layout_from_an_older_schema_is_refused_and_says_why() {
    let mut engine = test_engine();
    let mut memory = DisplayMemory::default();
    let mut window_store = WindowStore::default();
    let space = SpaceId::new(4615);
    let _ = engine.handle_event(
        &mut window_store,
        &mut memory,
        LayoutEvent::SpaceExposed(space, CGSize::new(1200.0, 800.0)),
    );
    engine.update_space_display(&mut memory, space, Some("studio-display".into()));

    let written = engine.serialize_to_string(&memory);
    let v5 = written.replacen(r#""schema_version":6"#, r#""schema_version":5"#, 1);
    assert_ne!(v5, written);

    let loaded = LayoutEngine::deserialize_file(&v5).expect("the bytes still parse");

    let error = match loaded.layout {
        Ok(_) => panic!("an older schema is refused"),
        Err(error) => error.to_string(),
    };
    assert!(
        error.contains("version 5 is older than 6")
            && error.contains("per display arrangement")
            && error.contains("laid out fresh"),
        "the error has to say what is wrong and what follows: {error}"
    );
    assert_eq!(
        loaded.memory.affinity.space_for_display(&DisplayUuid::new("studio-display")),
        Some(space),
        "the refusal is about the layout"
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
        "the current schema must not write the tag itself: {current}"
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
        message.contains("predates schema 4") && message.contains("laid out fresh"),
        "the error has to say what happened and what follows: {message}"
    );
}

/// The reason the display memory is its own record.
///
/// A layout written by a NEWER schema cannot be trusted, and rini refuses it. The refusal must keep
/// the machine's memory: lost with it, every window would be re-homed from scratch on the next
/// display change and every relaunched application land in a default slot. Which monitor a window
/// lives on is not a fact the layout's version has any bearing on.
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
    engine.update_space_display(&mut memory, space, Some("studio-display".into()));
    memory.affinity.set_window_home(window, &DisplayUuid::new("studio-display"));

    let written = engine.serialize_to_string(&memory);
    let from_the_future = written.replacen(r#""schema_version":6"#, r#""schema_version":99"#, 1);
    assert_ne!(from_the_future, written);

    let loaded = LayoutEngine::deserialize_file(&from_the_future).expect("the bytes still parse");

    let error = match loaded.layout {
        Ok(_) => panic!("a newer schema is refused"),
        Err(error) => error.to_string(),
    };
    assert!(error.contains("newer than supported"), "{error}");
    assert_eq!(
        loaded.memory.affinity.window_home(window),
        Some(&DisplayUuid::new("studio-display")),
        "the refusal is about the layout, not about which monitor the window lives on"
    );
    assert_eq!(
        loaded.memory.affinity.space_for_display(&DisplayUuid::new("studio-display")),
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
    engine.update_space_display(&mut memory, space, Some("studio-display".into()));

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
        loaded.memory.affinity.space_for_display(&DisplayUuid::new("studio-display")),
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
    engine.persistence.windows.insert(window, WindowFingerprint {
        window_server_id: Some(6001),
        title: Some("Invalid geometry".into()),
        width: -1.0,
        height: 500.0,
        app_id: Some("com.example.invalid".into()),
    });

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

/// A memory section missing a record reads it as empty: a file that remembers less, not one that
/// cannot be read (`display_memory.rs`).
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
    let (_dir, path) = layout_file();
    engine.save(&memory, path.clone()).unwrap();

    // Strip the record back out.
    let written = std::fs::read_to_string(&path).unwrap();
    let stripped = written.replace(",launch:(apps:{})", "");
    assert_ne!(
        stripped, written,
        "the record is written, so this test is checking something"
    );
    std::fs::write(&path, stripped).unwrap();

    let loaded = LayoutEngine::load_file(&path).unwrap();
    assert!(
        loaded.layout.is_ok(),
        "a missing record is a default, not a refusal"
    );
    assert!(loaded.memory.launch.is_empty());
}

/// Whether the strip's own shape survives the trip: with one layout system, which system came back
/// out of the file is no question.
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

/// A file schema 6 wrote, one with a column at each width, a full-width window, a fold, the widths
/// each display remembers and a display that showed another workspace before this one.
const SCHEMA_6_FILE: &str = include_str!("layout-schema-6.ron");

/// The file loads as the layout it describes, and writes back the same.
#[test]
fn a_schema_6_layout_file_loads_and_writes_back_the_same() {
    use crate::layout::ColumnWidth;

    const DISPLAY: &str = "37D8832A-2D66-02CA-B9F7-8F30A301B230";
    let loaded = LayoutEngine::deserialize_file(SCHEMA_6_FILE).expect("the bytes parse");
    let engine = loaded.layout.expect("the layout loads");
    let space = SpaceId::new(52);
    let w = |idx| WindowId::new(901, idx);
    let shown = engine.virtual_workspace_manager.active_workspace(space).expect("a workspace");
    let layout = engine.workspace_layouts.active(space, shown).expect("a layout");
    let tree = engine.workspace_tree(shown);
    let widths: Vec<_> = (1..=6).map(|idx| tree.carried_width(layout, w(idx))).collect();
    assert_eq!(widths, [
        None,
        Some(ColumnWidth::Offset(-0.1)),
        Some(ColumnWidth::Unbounded(0.33797)),
        Some(ColumnWidth::FullWidth),
        None,
        None,
    ]);
    let remembered: Vec<_> = (2..=4)
        .map(|idx| loaded.memory.affinity.window_width(&DisplayUuid::new(DISPLAY), w(idx)))
        .collect();
    assert_eq!(remembered, [
        Some(ColumnWidth::Offset(-0.1)),
        Some(ColumnWidth::Unbounded(0.33797)),
        Some(ColumnWidth::FullWidth),
    ]);
    let before = engine.virtual_workspace_manager.last_workspace(space).expect("one before");
    assert_ne!(before, shown);

    let rewritten = engine.serialize_to_string(&loaded.memory);
    let value = |text: &str| ron::from_str::<ron::Value>(text).expect("a RON value");
    assert_eq!(value(&rewritten), value(SCHEMA_6_FILE));
}

/// The bytes schema 6 writes after loading `SCHEMA_6_FILE`. Its maps come out in hash order, which
/// is not the file's own order, so the two files differ while holding the same values.
const SCHEMA_6_RESAVED: &str = include_str!("layout-schema-6-resaved.ron");

/// Every display UUID in the file is read and written as the bare string it was, so the file is
/// written back to the byte.
#[test]
fn a_schema_6_layout_file_writes_back_the_bytes_schema_6_writes() {
    let loaded = LayoutEngine::deserialize_file(SCHEMA_6_FILE).expect("the bytes parse");
    let engine = loaded.layout.expect("the layout loads");
    let rewritten = engine.serialize_to_string(&loaded.memory);
    assert_eq!(rewritten, SCHEMA_6_RESAVED);
    let display = DisplayUuid::new("37D8832A-2D66-02CA-B9F7-8F30A301B230");
    assert_eq!(
        loaded.memory.affinity.space_for_display(&display),
        Some(SpaceId::new(52))
    );
    let alone = crate::workspaces::domain::display_setup::SetupId::of([&display]);
    assert_eq!(
        loaded.memory.launch.slots("com.mitchellh.ghostty", &alone).len(),
        6
    );
    assert_eq!(
        rewritten.matches(r#""37D8832A-2D66-02CA-B9F7-8F30A301B230""#).count(),
        16
    );
}

/// `SCHEMA_6_FILE` as this rini writes it with applications' widths remembered: a terminal at two
/// thirds on the 16" built-in and a third on the 32", a browser full width and an editor dragged
/// past the bounds, all under Kai's half default.
const SCHEMA_6_APP_WIDTHS: &str = include_str!("layout-schema-6-app-widths.ron");

/// What `DisplayMemory` and the file were to the rini before applications' widths were kept, so a
/// test can read a new file as going back to that rini would.
mod before_app_widths {
    use serde::Deserialize;

    use super::super::{FloatingManager, PersistenceState, WorkspaceLayouts};
    use crate::workspaces::domain::display_affinity::DisplayAffinity;
    use crate::workspaces::domain::launch_memory::LaunchMemory;
    use crate::workspaces::{FloatingPositionStore, WorkspaceStore};

    #[derive(Default, Deserialize)]
    pub struct DisplayMemory {
        #[serde(default)]
        pub affinity: DisplayAffinity,
        #[serde(default)]
        pub launch: LaunchMemory,
    }

    #[derive(Deserialize)]
    pub struct PersistedLayout {
        #[serde(default)]
        pub schema_version: u32,
        pub workspace_layouts: WorkspaceLayouts,
        pub floating: FloatingManager,
        pub floating_positions: FloatingPositionStore,
        pub virtual_workspace_manager: WorkspaceStore,
        #[serde(default)]
        pub display_memory: DisplayMemory,
        #[serde(flatten)]
        pub persistence: PersistenceState,
    }
}

/// A rini from before applications' widths were kept reads a file that holds them, layout, homes,
/// widths and launch slots and all: one that could not would start with none and write that over
/// the file (`specs/displays.md`).
#[test]
fn a_file_with_app_widths_is_read_whole_by_the_rini_before_them() {
    use crate::layout::ColumnWidth;

    let old: before_app_widths::PersistedLayout =
        ron::from_str(SCHEMA_6_APP_WIDTHS).expect("the rini before app widths reads it");
    let display = DisplayUuid::new("37D8832A-2D66-02CA-B9F7-8F30A301B230");
    assert_eq!(old.schema_version, 6);
    assert_eq!(
        old.display_memory.affinity.space_for_display(&display),
        Some(SpaceId::new(52))
    );
    assert_eq!(
        old.display_memory.affinity.window_width(&display, WindowId::new(901, 4)),
        Some(ColumnWidth::FullWidth)
    );
    let alone = crate::workspaces::domain::display_setup::SetupId::of([&display]);
    assert_eq!(
        old.display_memory.launch.slots("com.mitchellh.ghostty", &alone).len(),
        6
    );
    assert_eq!(old.persistence.windows.len(), 6);
    assert_eq!(old.virtual_workspace_manager.workspace_count(), 4);
    assert_eq!(old.workspace_layouts.spaces().len(), 1);
    let (_, _) = (&old.floating, &old.floating_positions);
}

/// The widths the file holds are read back as they were written, and written back the same, its
/// maps in their own order.
#[test]
fn a_file_with_app_widths_reads_them_back_and_writes_them_back_the_same() {
    use crate::displays::domain::screen::DisplayShape;
    use crate::workspaces::domain::app_widths::{AppWidth, Overrode};

    let loaded = LayoutEngine::deserialize_file(SCHEMA_6_APP_WIDTHS).expect("the bytes parse");
    let engine = loaded.layout.expect("the layout loads");
    let half = Overrode::DefaultColumnWidth(0.5);
    let sixteen = DisplayShape {
        width_pt: 1728,
        height_pt: 1085,
        width_cm: 34,
    };
    let thirty_two = DisplayShape {
        width_pt: 3008,
        height_pt: 1660,
        width_cm: 70,
    };
    let widths = &loaded.memory.app_widths;
    let cases = [
        ("com.mitchellh.ghostty", sixteen, AppWidth::Bounded(0.66667)),
        ("com.mitchellh.ghostty", thirty_two, AppWidth::Bounded(0.33333)),
        ("com.google.Chrome", sixteen, AppWidth::FullWidth),
        ("com.microsoft.VSCode", sixteen, AppWidth::Unbounded(0.83797)),
    ];
    for (app, shape, width) in cases {
        assert_eq!(widths.width(app, shape, half), Some(width), "{app} on {shape:?}");
    }
    let value = |text: &str| ron::from_str::<ron::Value>(text).expect("a RON value");
    assert_eq!(
        value(&engine.serialize_to_string(&loaded.memory)),
        value(SCHEMA_6_APP_WIDTHS)
    );
}

/// A file from before applications' widths were kept loads with none, and is written back as it
/// was: an empty record adds nothing to the file.
#[test]
fn a_file_from_before_app_widths_loads_with_none_and_writes_back_without_them() {
    let loaded = LayoutEngine::deserialize_file(SCHEMA_6_FILE).expect("the bytes parse");
    assert!(loaded.memory.app_widths.is_empty());
    let engine = loaded.layout.expect("the layout loads");
    let rewritten = engine.serialize_to_string(&loaded.memory);
    assert!(!rewritten.contains("app_widths"), "{rewritten}");
}
