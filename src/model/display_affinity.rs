//! Durable display identity, and which display each window belongs to.
//!
//! macOS mints a NEW native space id every time a display is reconnected — observed
//! 479 -> 484 -> 487 -> 516 -> 550 -> 552 for a single monitor across one session — so
//! a space id is not a durable name for "the layout belonging to this monitor". The
//! display UUID is, and this type is the only place that knows the mapping between the
//! two.
//!
//! It also records, per window, which display that window belongs to. That is separate
//! from where macOS has currently parked the window: unplugging a display evacuates its
//! windows onto whatever display remains, and without a durable record of where they
//! came from there is nothing to consult on replug. Affinity is therefore only written
//! by paths that express intent (an explicit move, or first sighting of a window) and
//! never by the forced reassignment that follows a display change.

use serde::{Deserialize, Serialize};

use crate::actor::app::{WindowId, pid_t};
use crate::common::collections::HashMap;
use crate::sys::screen::SpaceId;

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct DisplayAffinity {
    /// Native space each display owns, by display UUID.
    ///
    /// Retained for every display seen in the session, including ones currently
    /// unplugged: that retained entry is the only thing that lets a replug find the
    /// layout it had before. Pruning it on unplug is what made reconnected displays
    /// come back empty.
    display_space: HashMap<String, SpaceId>,
    /// Display each window belongs to, by display UUID.
    window_home: HashMap<WindowId, String>,
}

impl DisplayAffinity {
    /// Record that `display` currently owns `space`.
    ///
    /// A native space belongs to exactly one display (Rift requires "Displays have
    /// separate Spaces"), so any other display previously claiming `space` is stale and
    /// is dropped. Without that eviction two displays can both appear to own one space
    /// and the affinity pass moves windows between them forever.
    pub fn set_display_space(&mut self, display: &str, space: SpaceId) {
        self.display_space.retain(|uuid, owned| *owned != space || uuid == display);
        self.display_space.insert(display.to_owned(), space);
    }

    pub fn space_for_display(&self, display: &str) -> Option<SpaceId> {
        self.display_space.get(display).copied()
    }

    pub fn display_for_space(&self, space: SpaceId) -> Option<&str> {
        self.display_space
            .iter()
            .find_map(|(uuid, owned)| (*owned == space).then_some(uuid.as_str()))
    }

    pub fn knows_display(&self, display: &str) -> bool {
        self.display_space.contains_key(display)
    }

    /// Move every record naming `old_space` onto `new_space`.
    pub fn remap_space(&mut self, old_space: SpaceId, new_space: SpaceId) {
        if old_space == new_space {
            return;
        }
        // The display arriving on new_space wins over whatever previously claimed it, for
        // the same one-display-per-space reason as set_display_space.
        let arriving: Vec<String> = self
            .display_space
            .iter()
            .filter(|(_, owned)| **owned == old_space)
            .map(|(uuid, _)| uuid.clone())
            .collect();
        if !arriving.is_empty() {
            self.display_space.retain(|_, owned| *owned != new_space);
        }
        for uuid in arriving {
            self.display_space.insert(uuid, new_space);
        }
    }

    /// Record that `window` belongs to `display`, replacing any previous home.
    ///
    /// Only for paths that express intent. The forced reassignment that follows a
    /// display change must not call this, or the evacuation overwrites the very record
    /// the replug needs.
    pub fn set_window_home(&mut self, window: WindowId, display: &str) {
        self.window_home.insert(window, display.to_owned());
    }

    /// Record a home only if the window does not already have one.
    ///
    /// Used at first sighting. A window that has been seen before keeps the display it
    /// was last deliberately placed on, even when it is currently parked elsewhere.
    pub fn set_window_home_if_absent(&mut self, window: WindowId, display: &str) {
        self.window_home.entry(window.to_owned()).or_insert_with(|| display.to_owned());
    }

    pub fn window_home(&self, window: WindowId) -> Option<&str> {
        self.window_home.get(&window).map(String::as_str)
    }

    pub fn windows_homed_to(&self, display: &str) -> Vec<WindowId> {
        let mut windows: Vec<WindowId> = self
            .window_home
            .iter()
            .filter_map(|(window, home)| (home == display).then_some(*window))
            .collect();
        windows.sort_unstable();
        windows
    }

    pub fn forget_window(&mut self, window: WindowId) {
        self.window_home.remove(&window);
    }

    pub fn forget_app(&mut self, pid: pid_t) {
        self.window_home.retain(|window, _| window.pid != pid);
    }

    /// Carry a window's home across an identity change (an app relaunching into a new
    /// `WindowId` for the same window).
    pub fn rekey_window(&mut self, from: WindowId, to: WindowId) {
        if let Some(home) = self.window_home.remove(&from) {
            self.window_home.insert(to, home);
        }
    }

    /// Adopt legacy persisted state from before this type existed.
    pub fn absorb_legacy(
        &mut self,
        space_display_map: HashMap<SpaceId, Option<String>>,
        display_last_space: HashMap<String, SpaceId>,
    ) {
        for (space, display) in space_display_map {
            if let Some(display) = display {
                self.set_display_space(&display, space);
            }
        }
        for (display, space) in display_last_space {
            // A legacy file carried both maps and they could disagree. The per-display
            // map was the one the reconnect path read, so let it win.
            self.set_display_space(&display, space);
        }
    }

    #[cfg(test)]
    pub fn homed_window_count(&self) -> usize {
        self.window_home.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn win(idx: u32) -> WindowId {
        WindowId::new(1, idx)
    }

    #[test]
    fn a_space_belongs_to_one_display_at_a_time() {
        let mut affinity = DisplayAffinity::default();
        affinity.set_display_space("built-in", SpaceId::new(1));
        // macOS handed space 1 to the external. The built-in must not still claim it.
        affinity.set_display_space("external", SpaceId::new(1));

        assert_eq!(affinity.space_for_display("external"), Some(SpaceId::new(1)));
        assert_eq!(affinity.space_for_display("built-in"), None);
        assert_eq!(affinity.display_for_space(SpaceId::new(1)), Some("external"));
    }

    #[test]
    fn reconnect_keeps_the_display_known_under_its_new_space_id() {
        let mut affinity = DisplayAffinity::default();
        affinity.set_display_space("external", SpaceId::new(479));
        affinity.remap_space(SpaceId::new(479), SpaceId::new(552));

        assert_eq!(affinity.space_for_display("external"), Some(SpaceId::new(552)));
        assert_eq!(affinity.display_for_space(SpaceId::new(479)), None);
        assert!(affinity.knows_display("external"));
    }

    #[test]
    fn remap_does_not_leave_two_displays_on_the_target_space() {
        let mut affinity = DisplayAffinity::default();
        affinity.set_display_space("external", SpaceId::new(479));
        affinity.set_display_space("built-in", SpaceId::new(552));
        // The external comes back and macOS gives it the id the built-in was using.
        affinity.remap_space(SpaceId::new(479), SpaceId::new(552));

        assert_eq!(affinity.space_for_display("external"), Some(SpaceId::new(552)));
        assert_eq!(affinity.space_for_display("built-in"), None);
    }

    #[test]
    fn first_sighting_does_not_overwrite_a_deliberate_home() {
        let mut affinity = DisplayAffinity::default();
        affinity.set_window_home(win(1), "external");
        // Unplug parks the window on the built-in and it is seen there again.
        affinity.set_window_home_if_absent(win(1), "built-in");

        assert_eq!(affinity.window_home(win(1)), Some("external"));
        assert_eq!(affinity.windows_homed_to("external"), vec![win(1)]);
    }

    #[test]
    fn an_explicit_move_does_overwrite_the_home() {
        let mut affinity = DisplayAffinity::default();
        affinity.set_window_home(win(1), "external");
        affinity.set_window_home(win(1), "built-in");

        assert_eq!(affinity.window_home(win(1)), Some("built-in"));
        assert!(affinity.windows_homed_to("external").is_empty());
    }

    #[test]
    fn legacy_state_is_adopted_from_both_maps() {
        let mut space_display_map = HashMap::default();
        space_display_map.insert(SpaceId::new(7), Some("external".to_string()));
        let mut display_last_space = HashMap::default();
        display_last_space.insert("built-in".to_string(), SpaceId::new(1));

        let mut affinity = DisplayAffinity::default();
        affinity.absorb_legacy(space_display_map, display_last_space);

        assert_eq!(affinity.space_for_display("external"), Some(SpaceId::new(7)));
        assert_eq!(affinity.space_for_display("built-in"), Some(SpaceId::new(1)));
    }
}
