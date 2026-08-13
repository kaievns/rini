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
    /// Last observed strip order per display, so a replug can rebuild adjacency rather
    /// than repatriating in arbitrary id order.
    #[serde(default)]
    display_strip: HashMap<String, Vec<WindowId>>,
    /// Column width each window last had on each display, keyed by display UUID.
    ///
    /// Width belongs to the DISPLAY, not the workspace: a window sized to fill the
    /// built-in should stay that size across every workspace on the built-in, and adopt
    /// whatever it last had on the external when it moves there. A half-width column is
    /// a sensible default on a 2338pt-wide monitor and cramped on a 1728pt laptop panel,
    /// so one remembered width per window cannot serve both.
    ///
    /// Storing it here rather than in the layout tree is deliberate. A tree is per
    /// workspace, so a width living there is necessarily per workspace — which is the
    /// bug this fixes: moving a full-size window from workspace 1 to 2 to 3 made it
    /// half-size on 2 (which held other windows) and full again on 3 (which was empty),
    /// because nothing was stored at all and the width was being inferred from how many
    /// columns each workspace happened to contain.
    #[serde(default)]
    window_width: HashMap<String, HashMap<WindowId, ColumnWidth>>,
}

/// The width a window occupied, as the layout means it rather than in points.
///
/// Points would not survive a resolution change or a move between displays of different
/// widths, which is exactly when this record is consulted.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum ColumnWidth {
    /// Full viewport width, as `toggle_fullscreen_within_gaps` produces. Kept distinct
    /// from `Offset` because it is a MODE, not a ratio: it must stay full width on a
    /// display of any size, and it round-trips back to the preset width when toggled off.
    FullWidth,
    /// A deliberate width, as an offset from the configured `column_width_ratio`.
    Offset(f64),
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

    /// Windows homed to `display`, in the strip order last observed on it.
    ///
    /// Order matters on replug. Repatriating in `WindowId` order is effectively arbitrary,
    /// so two windows the user had kept side by side come back with unrelated windows
    /// between them. Windows with a remembered position come first, in that order;
    /// anything homed here without one follows, in id order for determinism.
    pub fn windows_homed_to(&self, display: &str) -> Vec<WindowId> {
        let mut homed: Vec<WindowId> = self
            .window_home
            .iter()
            .filter_map(|(window, home)| (home == display).then_some(*window))
            .collect();
        homed.sort_unstable();

        let order = self.display_strip.get(display);
        let mut ordered: Vec<WindowId> = Vec::with_capacity(homed.len());
        if let Some(order) = order {
            ordered.extend(order.iter().copied().filter(|window| homed.contains(window)));
        }
        let remainder: Vec<WindowId> =
            homed.into_iter().filter(|window| !ordered.contains(window)).collect();
        ordered.extend(remainder);
        ordered
    }

    /// Remember the strip order currently on `display`.
    ///
    /// Recorded continuously while the display is attached, so the last snapshot before an
    /// unplug is the arrangement the user actually left behind — including windows opened,
    /// dragged in, or reshuffled since they were first homed.
    pub fn set_display_strip(&mut self, display: &str, windows: Vec<WindowId>) {
        if windows.is_empty() {
            self.display_strip.remove(display);
        } else {
            self.display_strip.insert(display.to_owned(), windows);
        }
    }

    pub fn display_strip(&self, display: &str) -> &[WindowId] {
        self.display_strip.get(display).map(Vec::as_slice).unwrap_or_default()
    }

    /// Remember the width `window` occupies on `display`.
    pub fn set_window_width(&mut self, display: &str, window: WindowId, width: ColumnWidth) {
        self.window_width.entry(display.to_owned()).or_default().insert(window, width);
    }

    /// Forget any remembered width, so the window adopts the display's default.
    ///
    /// Distinct from never having had one: toggling a deliberate width back off is an
    /// instruction to stop pinning it, not to keep the old value.
    pub fn clear_window_width(&mut self, display: &str, window: WindowId) {
        if let Some(widths) = self.window_width.get_mut(display) {
            widths.remove(&window);
            if widths.is_empty() {
                self.window_width.remove(display);
            }
        }
    }

    /// The width `window` last had on `display`, if it ever had a deliberate one.
    pub fn window_width(&self, display: &str, window: WindowId) -> Option<ColumnWidth> {
        self.window_width.get(display)?.get(&window).copied()
    }

    /// Every window that currently has a home, in any display.
    pub fn homed_windows(&self) -> Vec<WindowId> {
        let mut windows: Vec<WindowId> = self.window_home.keys().copied().collect();
        windows.sort_unstable();
        windows
    }

    pub fn forget_window(&mut self, window: WindowId) {
        self.window_home.remove(&window);
        for strip in self.display_strip.values_mut() {
            strip.retain(|candidate| *candidate != window);
        }
        for widths in self.window_width.values_mut() {
            widths.remove(&window);
        }
        self.window_width.retain(|_, widths| !widths.is_empty());
    }

    pub fn forget_app(&mut self, pid: pid_t) {
        self.window_home.retain(|window, _| window.pid != pid);
        for strip in self.display_strip.values_mut() {
            strip.retain(|window| window.pid != pid);
        }
        for widths in self.window_width.values_mut() {
            widths.retain(|window, _| window.pid != pid);
        }
        self.window_width.retain(|_, widths| !widths.is_empty());
    }

    /// Carry a window's home across an identity change (an app relaunching into a new
    /// `WindowId` for the same window).
    pub fn rekey_window(&mut self, from: WindowId, to: WindowId) {
        if let Some(home) = self.window_home.remove(&from) {
            self.window_home.insert(to, home);
        }
        for strip in self.display_strip.values_mut() {
            for window in strip.iter_mut() {
                if *window == from {
                    *window = to;
                }
            }
        }
        for widths in self.window_width.values_mut() {
            if let Some(width) = widths.remove(&from) {
                widths.insert(to, width);
            }
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
    fn a_window_seen_on_a_new_display_is_re_homed_to_it() {
        let mut affinity = DisplayAffinity::default();
        affinity.set_window_home(win(1), "external");
        // Seen on the built-in while BOTH displays are attached: the user moved it, so the
        // built-in is its home now. Re-homing has to overwrite, not defer to the old value,
        // or a later replug of the external hauls the window back off the built-in.
        affinity.set_window_home(win(1), "built-in");

        assert_eq!(affinity.window_home(win(1)), Some("built-in"));
        assert!(affinity.windows_homed_to("external").is_empty());
    }

    #[test]
    fn strip_order_drives_repatriation_order() {
        let mut affinity = DisplayAffinity::default();
        // Ids deliberately out of visual order: sorting by WindowId would give 1, 2, 8, 9
        // and split the adjacent pair 8, 9 apart from where the user left them.
        for window in [win(1), win(8), win(9), win(2)] {
            affinity.set_window_home(window, "external");
        }
        affinity.set_display_strip("external", vec![win(1), win(8), win(9), win(2)]);

        assert_eq!(
            affinity.windows_homed_to("external"),
            vec![win(1), win(8), win(9), win(2)]
        );
    }

    /// The point of keying width by display: one window, two displays, two answers.
    #[test]
    fn a_window_can_have_a_different_width_on_each_display() {
        let mut affinity = DisplayAffinity::default();
        affinity.set_window_width("built-in", win(1), ColumnWidth::FullWidth);
        affinity.set_window_width("external", win(1), ColumnWidth::Offset(0.25));

        assert_eq!(
            affinity.window_width("built-in", win(1)),
            Some(ColumnWidth::FullWidth)
        );
        assert_eq!(
            affinity.window_width("external", win(1)),
            Some(ColumnWidth::Offset(0.25))
        );
        // A display it has never been on has no opinion, so the window adopts that
        // display's configured default rather than inheriting another display's size.
        assert_eq!(affinity.window_width("third", win(1)), None);
    }

    /// Clearing must forget, not freeze the last value: toggling a deliberate width off is an
    /// instruction to follow the display default again.
    #[test]
    fn clearing_a_width_restores_the_display_default() {
        let mut affinity = DisplayAffinity::default();
        affinity.set_window_width("built-in", win(1), ColumnWidth::FullWidth);
        affinity.clear_window_width("built-in", win(1));

        assert_eq!(affinity.window_width("built-in", win(1)), None);
    }

    #[test]
    fn forgetting_a_window_drops_its_remembered_widths() {
        let mut affinity = DisplayAffinity::default();
        affinity.set_window_width("built-in", win(1), ColumnWidth::FullWidth);
        affinity.set_window_width("built-in", win(2), ColumnWidth::Offset(0.1));
        affinity.forget_window(win(1));

        assert_eq!(affinity.window_width("built-in", win(1)), None);
        assert_eq!(
            affinity.window_width("built-in", win(2)),
            Some(ColumnWidth::Offset(0.1)),
            "forgetting one window must not disturb another"
        );
    }

    /// An app relaunching into a new WindowId must keep the size the user gave it, for the
    /// same reason its home display carries across.
    #[test]
    fn rekeying_carries_remembered_widths() {
        let mut affinity = DisplayAffinity::default();
        affinity.set_window_width("built-in", win(1), ColumnWidth::FullWidth);
        affinity.rekey_window(win(1), win(7));

        assert_eq!(affinity.window_width("built-in", win(1)), None);
        assert_eq!(
            affinity.window_width("built-in", win(7)),
            Some(ColumnWidth::FullWidth)
        );
    }

    #[test]
    fn forgetting_an_app_drops_widths_for_all_of_its_windows() {
        let mut affinity = DisplayAffinity::default();
        affinity.set_window_width("built-in", WindowId::new(1, 1), ColumnWidth::FullWidth);
        affinity.set_window_width("built-in", WindowId::new(1, 2), ColumnWidth::Offset(0.2));
        affinity.set_window_width("built-in", WindowId::new(2, 1), ColumnWidth::Offset(0.3));
        affinity.forget_app(1);

        assert_eq!(affinity.window_width("built-in", WindowId::new(1, 1)), None);
        assert_eq!(affinity.window_width("built-in", WindowId::new(1, 2)), None);
        assert_eq!(
            affinity.window_width("built-in", WindowId::new(2, 1)),
            Some(ColumnWidth::Offset(0.3)),
            "another app's windows must survive"
        );
    }

    #[test]
    fn a_window_homed_without_a_remembered_position_still_comes_back() {
        let mut affinity = DisplayAffinity::default();
        affinity.set_window_home(win(1), "external");
        affinity.set_window_home(win(5), "external");
        // Only one of them has a position; the other must not be silently dropped.
        affinity.set_display_strip("external", vec![win(5)]);

        assert_eq!(affinity.windows_homed_to("external"), vec![win(5), win(1)]);
    }

    #[test]
    fn forgetting_a_window_also_drops_it_from_every_strip() {
        let mut affinity = DisplayAffinity::default();
        affinity.set_window_home(win(1), "external");
        affinity.set_display_strip("external", vec![win(1), win(2)]);
        affinity.forget_window(win(1));

        assert_eq!(affinity.display_strip("external"), &[win(2)]);
        assert_eq!(affinity.window_home(win(1)), None);
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
