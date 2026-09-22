//! Where an application's windows belong, under a key that survives the application.
//!
//! `DisplayAffinity` already records which display a window belongs to and what width it had there,
//! and the workspace manager records which workspace it was in. All of it is keyed by
//! `WindowId { pid, idx }`, so all of it is dead weight the moment the application quits: a relaunched
//! window arrives as a default-width column in whatever workspace happens to be active.
//!
//! This holds the same three facts keyed by bundle identifier and display topology instead. See
//! `src/workspaces/docs/launch-memory.md` (in this crate).

use serde::{Deserialize, Serialize};

use rustc_hash::FxHashMap as HashMap;
use crate::workspaces::domain::display_affinity::ColumnWidth;

/// The set of displays connected, as a name that can key a map.
///
/// Sorted, so the same set of displays always produces the same key however the window server happens
/// to enumerate them.
pub fn topology_key(display_uuids: &[String]) -> String {
    let mut sorted: Vec<&str> = display_uuids.iter().map(String::as_str).collect();
    sorted.sort_unstable();
    sorted.join("+")
}

/// Where one window of an application was, the last time this topology was connected.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Slot {
    /// Title when it was recorded, for matching a relaunched window back to its own slot. Absent when
    /// the window had no title.
    #[serde(default)]
    pub title: Option<String>,
    pub display_uuid: String,
    pub workspace_index: usize,
    /// Absent when the window never had a deliberate width, in which case the configured default is
    /// still the right answer and nothing needs remembering.
    #[serde(default)]
    pub width: Option<ColumnWidth>,
}

/// What a projection could work out about one window's width.
///
/// The distinction is the whole point. A width that could not be READ is not a window without a
/// width, and treating the two alike is what erased a remembered full-width window on the next
/// autosave, so it relaunched at the default half width. See `src/workspaces/docs/launch-memory.md`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ProjectedWidth {
    /// The window's own layout answered. `None` is a real answer: the display's configured default.
    Known(Option<ColumnWidth>),
    /// No layout holding this window could be consulted, so what is already remembered must stand.
    Unknown,
}

/// A slot as a projection produces it, before its unknowns are resolved against what is remembered.
#[derive(Debug, Clone, PartialEq)]
pub struct ProjectedSlot {
    pub title: Option<String>,
    pub display_uuid: String,
    pub workspace_index: usize,
    pub width: ProjectedWidth,
}

/// Fixtures say what a slot's width IS, so they project a known one. Test-only: production builds a
/// `ProjectedSlot` from the layout, which is the one place allowed to decide the width is unknown.
#[cfg(test)]
impl From<Slot> for ProjectedSlot {
    fn from(slot: Slot) -> Self {
        Self {
            title: slot.title,
            display_uuid: slot.display_uuid,
            workspace_index: slot.workspace_index,
            width: ProjectedWidth::Known(slot.width),
        }
    }
}

/// The width to store for one projected slot, given what the application already had remembered.
///
/// `Known` is taken as it stands, including a deliberate `None` for "back to the display default".
/// `Unknown` inherits: by title first, since a title is what identifies a window across a relaunch,
/// then by position for the applications whose titles change every session.
pub fn resolve_width(
    projected: &ProjectedSlot,
    index: usize,
    remembered: &[Slot],
) -> Option<ColumnWidth> {
    match projected.width {
        ProjectedWidth::Known(width) => width,
        ProjectedWidth::Unknown => projected
            .title
            .as_deref()
            .and_then(|title| {
                remembered.iter().find(|old| old.title.as_deref() == Some(title))
            })
            .or_else(|| remembered.get(index))
            .and_then(|old| old.width),
    }
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct LaunchMemory {
    /// Slots per application, per display topology.
    #[serde(default)]
    apps: HashMap<String, HashMap<String, Vec<Slot>>>,
}

impl LaunchMemory {
    /// Replaces everything remembered about `app_id` under `topology`.
    ///
    /// A whole-set replacement rather than a merge: the slots are a projection of the windows the
    /// application had, so a window that is gone should not leave a slot behind for the next launch to
    /// match against.
    ///
    /// An EMPTY set is ignored rather than treated as a replacement. A projection that produced nothing
    /// means the windows could not be read, not that the application is meant to be forgotten — and
    /// forgetting is the one outcome that cannot be recovered from, since the whole point is to hold the
    /// arrangement across the application not running at all.
    ///
    /// The same rule applies per FIELD, not just per application: a slot whose width the projection
    /// could not work out keeps the width already remembered (`resolve_width`). Replacing the set
    /// wholesale with `width: None` is how a full-width window came back half sized.
    pub fn remember(&mut self, app_id: &str, topology: &str, projected: Vec<ProjectedSlot>) {
        if projected.is_empty() {
            return;
        }
        let remembered = self.slots(app_id, topology).to_vec();
        let slots: Vec<Slot> = projected
            .iter()
            .enumerate()
            .map(|(index, slot)| Slot {
                title: slot.title.clone(),
                display_uuid: slot.display_uuid.clone(),
                workspace_index: slot.workspace_index,
                width: resolve_width(slot, index, &remembered),
            })
            .collect();
        self.apps
            .entry(app_id.to_owned())
            .or_default()
            .insert(topology.to_owned(), slots);
    }

    pub fn slots(&self, app_id: &str, topology: &str) -> &[Slot] {
        self.apps
            .get(app_id)
            .and_then(|topologies| topologies.get(topology))
            .map(Vec::as_slice)
            .unwrap_or_default()
    }

    pub fn is_empty(&self) -> bool {
        self.apps.is_empty()
    }
}

/// Which remembered slot a newly appeared window should take.
///
/// `claimed` is the set of slot indices already handed to other windows of the same application, so two
/// windows cannot take the same one. `ordinal` is how many windows of this application have been placed
/// before this one.
///
/// Title first, because it is the only thing that identifies a particular window: a terminal's title is
/// its working directory, an editor's is its project. Ordinal second, for applications whose titles are
/// page titles and change every session. Nothing at all for a window beyond the remembered slots, which
pub fn slot_for_window(
    slots: &[Slot],
    title: Option<&str>,
    ordinal: usize,
    claimed: &[usize],
) -> Option<usize> {
    let free = |index: usize| !claimed.contains(&index);

    if let Some(title) = title.filter(|title| !title.is_empty()) {
        let by_title = slots
            .iter()
            .enumerate()
            .position(|(index, slot)| free(index) && slot.title.as_deref() == Some(title));
        if let Some(index) = by_title {
            return Some(index);
        }
    }

    // The ordinal names a slot directly; it is only usable if nothing else has taken it.
    (ordinal < slots.len() && free(ordinal)).then_some(ordinal)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn slot(title: &str, display: &str, workspace: usize) -> Slot {
        Slot {
            title: Some(title.to_owned()),
            display_uuid: display.to_owned(),
            workspace_index: workspace,
            width: None,
        }
    }

    const BUILT_IN: &str = "37D8832A-2D66-02CA-B9F7-8F30A301B230";
    const EXTERNAL: &str = "B9F7-8F30A301B230-37D8832A-2D66";

    #[test]
    fn the_same_displays_in_any_order_are_one_topology() {
        let one = topology_key(&[BUILT_IN.to_owned(), EXTERNAL.to_owned()]);
        let other = topology_key(&[EXTERNAL.to_owned(), BUILT_IN.to_owned()]);
        assert_eq!(one, other);
    }

    /// The case the whole thing exists for: half width on the external while it is plugged in, full width
    /// on the built-in when it is not, and the answer depends on which is connected.
    #[test]
    fn each_topology_remembers_its_own_answer() {
        let docked = topology_key(&[BUILT_IN.to_owned(), EXTERNAL.to_owned()]);
        let alone = topology_key(&[BUILT_IN.to_owned()]);
        let mut memory = LaunchMemory::default();

        memory.remember(
            "com.mitchellh.ghostty",
            &docked,
            vec![Slot {
                title: None,
                display_uuid: EXTERNAL.to_owned(),
                workspace_index: 2,
                width: Some(ColumnWidth::Offset(0.0)),
            }.into()],
        );
        memory.remember(
            "com.mitchellh.ghostty",
            &alone,
            vec![Slot {
                title: None,
                display_uuid: BUILT_IN.to_owned(),
                workspace_index: 0,
                width: Some(ColumnWidth::FullWidth),
            }.into()],
        );

        let docked_slots = memory.slots("com.mitchellh.ghostty", &docked);
        assert_eq!(docked_slots[0].display_uuid, EXTERNAL);
        assert_eq!(docked_slots[0].width, Some(ColumnWidth::Offset(0.0)));

        let alone_slots = memory.slots("com.mitchellh.ghostty", &alone);
        assert_eq!(alone_slots[0].display_uuid, BUILT_IN);
        assert_eq!(alone_slots[0].width, Some(ColumnWidth::FullWidth));
    }

    #[test]
    fn a_topology_that_was_never_seen_remembers_nothing() {
        let mut memory = LaunchMemory::default();
        memory.remember("app", "one-display", vec![slot("t", BUILT_IN, 0).into()]);
        assert!(memory.slots("app", "two-displays").is_empty());
        assert!(memory.slots("other-app", "one-display").is_empty());
    }

    /// Titles are what identify a particular window of a multi-window application, so they are tried
    /// first and the ordinal is not allowed to override them.
    #[test]
    fn a_window_takes_the_slot_with_its_own_title() {
        let slots = vec![
            slot("~/projects/rini", BUILT_IN, 0),
            slot("~/w/status", BUILT_IN, 2),
            slot("~/tmp", BUILT_IN, 3),
        ];
        assert_eq!(slot_for_window(&slots, Some("~/w/status"), 0, &[]), Some(1));
        assert_eq!(slot_for_window(&slots, Some("~/tmp"), 1, &[]), Some(2));
    }

    /// Chrome's titles are page titles and change every session, so the Nth window takes the Nth slot.
    #[test]
    fn a_window_with_an_unrecognised_title_falls_back_to_its_ordinal() {
        let slots = vec![slot("Inbox", BUILT_IN, 0), slot("Some ticket", BUILT_IN, 2)];
        assert_eq!(slot_for_window(&slots, Some("A page nobody saved"), 0, &[]), Some(0));
        assert_eq!(slot_for_window(&slots, Some("Another new page"), 1, &[]), Some(1));
    }

    #[test]
    fn a_window_with_no_title_falls_back_to_its_ordinal() {
        let slots = vec![slot("Inbox", BUILT_IN, 0), slot("Drafts", BUILT_IN, 2)];
        assert_eq!(slot_for_window(&slots, None, 1, &[]), Some(1));
        assert_eq!(slot_for_window(&slots, Some(""), 0, &[]), Some(0));
    }

    /// Two windows must not land on one slot, or they end up in the same workspace at the same width and
    /// the remembered arrangement collapses.
    #[test]
    fn a_claimed_slot_is_not_handed_out_twice() {
        let slots = vec![slot("Inbox", BUILT_IN, 0), slot("Drafts", BUILT_IN, 2)];
        assert_eq!(slot_for_window(&slots, Some("Inbox"), 0, &[0]), None, "ordinal 0 is taken too");
        assert_eq!(slot_for_window(&slots, Some("Inbox"), 1, &[0]), Some(1), "so it takes its ordinal");
    }

    #[test]
    fn a_window_beyond_the_remembered_ones_gets_no_slot() {
        let slots = vec![slot("Inbox", BUILT_IN, 0)];
        assert_eq!(slot_for_window(&slots, Some("Something new"), 1, &[0]), None);
        assert_eq!(slot_for_window(&[], Some("Anything"), 0, &[]), None);
    }

    /// The slots are a projection of the windows an application had. A window that is gone must not leave
    /// a slot behind for the next launch to match against.
    #[test]
    fn remembering_replaces_rather_than_accumulates() {
        let mut memory = LaunchMemory::default();
        memory.remember("app", "one", vec![slot("a", BUILT_IN, 0).into(), slot("b", BUILT_IN, 1).into()]);
        memory.remember("app", "one", vec![slot("a", BUILT_IN, 0).into()]);
        assert_eq!(memory.slots("app", "one").len(), 1);
    }

    /// The projection of an application whose windows are all in unseen workspaces produced
    /// nothing; wiping the entry on that is the one unrecoverable outcome.
    #[test]
    fn remembering_nothing_leaves_what_is_known_alone() {
        let mut memory = LaunchMemory::default();
        memory.remember("app", "one", vec![slot("a", BUILT_IN, 0).into()]);
        memory.remember("app", "one", Vec::new());
        assert_eq!(memory.slots("app", "one").len(), 1, "the entry survives an empty projection");
    }

    fn projected(title: Option<&str>, width: ProjectedWidth) -> ProjectedSlot {
        ProjectedSlot {
            title: title.map(str::to_owned),
            display_uuid: BUILT_IN.to_owned(),
            workspace_index: 0,
            width,
        }
    }

    fn remembered(title: Option<&str>, width: Option<ColumnWidth>) -> Slot {
        Slot {
            title: title.map(str::to_owned),
            display_uuid: BUILT_IN.to_owned(),
            workspace_index: 0,
            width,
        }
    }

    // The bug this type exists for. The projection could not read the layout, so it reported
    // `Unknown`; treating that as "no width" is what sent a full-width window back at half size.
    #[test]
    fn a_width_that_could_not_be_read_keeps_the_one_remembered() {
        let old = [remembered(Some("~/projects/rini"), Some(ColumnWidth::FullWidth))];
        let new = projected(Some("~/projects/rini"), ProjectedWidth::Unknown);
        assert_eq!(resolve_width(&new, 0, &old), Some(ColumnWidth::FullWidth));
    }

    // The other half: toggling back to the default has to be recorded, or ctrl-F could never be
    // undone across a relaunch. `Known(None)` is a deliberate answer and overwrites.
    #[test]
    fn a_width_read_as_the_default_overwrites_a_remembered_one() {
        let old = [remembered(Some("term"), Some(ColumnWidth::FullWidth))];
        let new = projected(Some("term"), ProjectedWidth::Known(None));
        assert_eq!(resolve_width(&new, 0, &old), None);
    }

    #[test]
    fn an_unknown_width_inherits_by_title_before_position() {
        let old = [
            remembered(Some("other"), Some(ColumnWidth::Offset(0.25))),
            remembered(Some("mine"), Some(ColumnWidth::FullWidth)),
        ];
        // Position 0 would give the offset; the title says otherwise.
        let new = projected(Some("mine"), ProjectedWidth::Unknown);
        assert_eq!(resolve_width(&new, 0, &old), Some(ColumnWidth::FullWidth));
    }

    // An application whose titles change every session, so only the ordinal can match.
    #[test]
    fn an_unknown_width_falls_back_to_the_slot_in_the_same_position() {
        let old = [
            remembered(Some("page one"), None),
            remembered(Some("page two"), Some(ColumnWidth::FullWidth)),
        ];
        let new = projected(Some("page three"), ProjectedWidth::Unknown);
        assert_eq!(resolve_width(&new, 1, &old), Some(ColumnWidth::FullWidth));
    }

    #[test]
    fn an_unknown_width_with_nothing_remembered_is_no_width() {
        let new = projected(Some("term"), ProjectedWidth::Unknown);
        assert_eq!(resolve_width(&new, 0, &[]), None);
    }

    /// The autosave loop that erased the record: every save re-projects, and one save that could not
    /// read the layout used to overwrite the width with `None` for good.
    #[test]
    fn repeated_saves_that_cannot_read_the_layout_never_erase_a_width() {
        let mut memory = LaunchMemory::default();
        memory.remember(
            "app",
            "one",
            vec![projected(Some("term"), ProjectedWidth::Known(Some(ColumnWidth::FullWidth)))],
        );
        for _ in 0..5 {
            memory.remember("app", "one", vec![projected(Some("term"), ProjectedWidth::Unknown)]);
            assert_eq!(
                memory.slots("app", "one")[0].width,
                Some(ColumnWidth::FullWidth),
                "an unreadable projection must not erase the width"
            );
        }
    }
}
