//! Where an application's windows belong, under a key that survives the application.
//!
//! `DisplayAffinity` already records which display a window belongs to and what width it had there,
//! and the workspace manager records which workspace it was in. All of it is keyed by
//! `WindowId { pid, idx }`, so all of it is dead weight the moment the application quits: a relaunched
//! window arrives as a default-width column in whatever workspace happens to be active.
//!
//! This holds the same three facts keyed by bundle identifier and display topology instead. See
//! `docs/launch-memory.md`.

use serde::{Deserialize, Serialize};

use crate::common::collections::HashMap;
use crate::model::display_affinity::ColumnWidth;

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
    pub fn remember(&mut self, app_id: &str, topology: &str, slots: Vec<Slot>) {
        if slots.is_empty() {
            if let Some(topologies) = self.apps.get_mut(app_id) {
                topologies.remove(topology);
                if topologies.is_empty() {
                    self.apps.remove(app_id);
                }
            }
            return;
        }
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

    /// Drops everything remembered for applications not in `keep`.
    ///
    /// Without this the file grows for every application ever launched. Called with the applications
    /// currently running, at save time.
    pub fn retain_apps(&mut self, keep: &dyn Fn(&str) -> bool) {
        self.apps.retain(|app_id, _| keep(app_id));
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
/// then gets the ordinary defaults.
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
            }],
        );
        memory.remember(
            "com.mitchellh.ghostty",
            &alone,
            vec![Slot {
                title: None,
                display_uuid: BUILT_IN.to_owned(),
                workspace_index: 0,
                width: Some(ColumnWidth::FullWidth),
            }],
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
        memory.remember("app", "one-display", vec![slot("t", BUILT_IN, 0)]);
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
        memory.remember("app", "one", vec![slot("a", BUILT_IN, 0), slot("b", BUILT_IN, 1)]);
        memory.remember("app", "one", vec![slot("a", BUILT_IN, 0)]);
        assert_eq!(memory.slots("app", "one").len(), 1);
    }

    #[test]
    fn remembering_nothing_forgets_the_application() {
        let mut memory = LaunchMemory::default();
        memory.remember("app", "one", vec![slot("a", BUILT_IN, 0)]);
        memory.remember("app", "one", Vec::new());
        assert!(memory.slots("app", "one").is_empty());
        assert!(memory.is_empty(), "and leaves nothing behind to grow the file");
    }

    #[test]
    fn applications_that_are_gone_are_dropped() {
        let mut memory = LaunchMemory::default();
        memory.remember("kept", "one", vec![slot("a", BUILT_IN, 0)]);
        memory.remember("dropped", "one", vec![slot("b", BUILT_IN, 0)]);
        memory.retain_apps(&|app_id| app_id == "kept");
        assert!(!memory.slots("kept", "one").is_empty());
        assert!(memory.slots("dropped", "one").is_empty());
    }
}
