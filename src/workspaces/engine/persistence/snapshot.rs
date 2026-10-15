use super::*;
use crate::workspaces::domain::display_memory::DisplayMemory;

/// 6 keys the window records by display ARRANGEMENT (`display_setup::SetupId`). It is also the
/// oldest schema read: an older layout is refused, and the bump is what makes an older rini refuse a
/// 6 file instead of reading it and finding no homes at all.
pub(super) const CURRENT_SCHEMA_VERSION: u32 = 6;

/// Owned, versioned representation of the layout file.
///
/// Keep this type independent from runtime-only `LayoutEngine` fields. Adding an engine cache,
/// service, or transient index must never silently alter the persistence schema again.
#[derive(Deserialize)]
pub(super) struct PersistedLayout {
    /// A file that names no version reads as 0, so it is refused as older rather than unparseable.
    #[serde(default)]
    pub(super) schema_version: u32,
    pub(super) workspace_layouts: WorkspaceLayouts,
    pub(super) floating: FloatingManager,
    pub(super) floating_positions: FloatingPositionStore,
    pub(super) virtual_workspace_manager: WorkspaceStore,
    /// What rini remembers about the machine, in its own section. Validated on its own, so a
    /// refused layout does not take it down: see `display_memory.rs`.
    #[serde(default)]
    pub(super) display_memory: DisplayMemory,
    #[serde(flatten)]
    pub(super) persistence: PersistenceState,
}

/// Borrowed serialization view, avoiding a deep clone of every layout tree during save.
#[derive(Serialize)]
struct PersistedLayoutRef<'a> {
    schema_version: u32,
    workspace_layouts: &'a WorkspaceLayouts,
    floating: &'a FloatingManager,
    floating_positions: &'a FloatingPositionStore,
    virtual_workspace_manager: &'a WorkspaceStore,
    display_memory: &'a DisplayMemory,
    #[serde(flatten)]
    persistence: &'a PersistenceState,
}

impl PersistedLayout {
    pub(super) fn deserialize(buf: &str) -> Result<Self, ron::error::SpannedError> {
        ron::from_str(buf)
    }

    pub(super) fn serialize_engine(engine: &LayoutEngine, memory: &DisplayMemory) -> String {
        ron::ser::to_string(&PersistedLayoutRef {
            schema_version: CURRENT_SCHEMA_VERSION,
            workspace_layouts: &engine.workspace_layouts,
            floating: &engine.floating,
            floating_positions: &engine.floating_positions,
            virtual_workspace_manager: &engine.virtual_workspace_manager,
            display_memory: memory,
            persistence: &engine.persistence,
        })
        .expect("persisted layout serialization must support all engine layout state")
    }

    /// The display memory this file carries.
    ///
    /// Read separately from the layout, and before it is validated, so a layout that cannot be
    /// trusted does not also cost the machine's memory of which monitor each window lives on.
    pub(super) fn take_display_memory(&mut self) -> DisplayMemory {
        std::mem::take(&mut self.display_memory)
    }

    pub(super) fn into_engine(self) -> LayoutEngine {
        LayoutEngine {
            workspace_layouts: self.workspace_layouts,
            floating: self.floating,
            floating_positions: self.floating_positions,
            app_rules: AppRuleEngine::default(),
            focused_window: None,
            window_layout_constraints: HashMap::default(),
            borders: Default::default(),
            virtual_workspace_manager: self.virtual_workspace_manager,
            layout_settings: LayoutSettings::default(),
            outbox: Default::default(),
            connected_displays: Vec::new(),
            persistence: self.persistence,
            startup_restore_pending: false,
        }
    }
}
