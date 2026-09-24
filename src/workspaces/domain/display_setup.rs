//! One hardware arrangement, and what it remembers about the windows.
//!
//! A window's place is not a property of a display. It is a property of the display SET: a browser
//! is full width on the laptop alone and half width beside an external, and a terminal that shares
//! the laptop screen with an editor when two monitors are attached wants the whole screen when the
//! editor has gone with the external. Keying these records by one display cannot express either,
//! because the display is the same in both arrangements.
//!
//! So each arrangement — [`SetupId`], the set of attached display UUIDs — gets its own [`Setup`]:
//! which display each window belongs to, the strip order per display, and the width per window and
//! display. Arrangements never share records. Rearranging with the external plugged in cannot
//! disturb how the laptop looks alone, which is the whole point.
//!
//! Identifying by UUID SET rather than by count matters: an external at home and one at the office
//! are different arrangements, and a projector in a meeting room is a third. None of them inherits
//! the others' layout, and a display nobody has seen before starts with no claim on any window.

use std::fmt;

use serde::{Deserialize, Serialize};

use rini_core::ids::{WindowId, pid_t};
use rustc_hash::FxHashMap as HashMap;

use crate::workspaces::domain::display_affinity::ColumnWidth;

/// The set of displays attached at one moment, as a durable name.
///
/// Sorted and deduplicated, so the order macOS happens to report the screens in cannot produce two
/// names for one arrangement — which would have made a replug a coin toss between two remembered
/// layouts.
#[derive(Debug, Default, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SetupId(String);

impl SetupId {
    /// The arrangement these display UUIDs form. Empty ones are ignored: a screen whose UUID macOS
    /// has not reported yet must not become part of the name, or the arrangement is renamed the
    /// moment the UUID arrives.
    pub fn of<'a>(displays: impl IntoIterator<Item = &'a str>) -> Self {
        let mut uuids: Vec<&str> =
            displays.into_iter().filter(|uuid| !uuid.trim().is_empty()).collect();
        uuids.sort_unstable();
        uuids.dedup();
        Self(uuids.join("+"))
    }

    /// No displays at all, which is what macOS reports mid-reconfiguration.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SetupId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// What one arrangement remembers.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Setup {
    /// Display each window belongs to, by display UUID.
    #[serde(default)]
    pub window_home: HashMap<WindowId, String>,
    /// Last observed strip order per display, so a replug rebuilds adjacency rather than
    /// repatriating in arbitrary id order.
    #[serde(default)]
    pub display_strip: HashMap<String, Vec<WindowId>>,
    /// Column width each window last had on each display.
    #[serde(default)]
    pub window_width: HashMap<String, HashMap<WindowId, ColumnWidth>>,
}

impl Setup {
    /// Whether this arrangement has any memory of the windows.
    ///
    /// The question a display change asks: an arrangement that remembers nothing must not move
    /// anything, because there is nothing to move windows TO and guessing is what throws them
    /// around the screen.
    pub fn is_empty(&self) -> bool {
        self.window_home.is_empty() && self.display_strip.is_empty() && self.window_width.is_empty()
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

    /// Windows homed to `display`, in the strip order last observed on it.
    ///
    /// Order matters on replug. Repatriating in `WindowId` order is effectively arbitrary, so two
    /// windows the user kept side by side come back with unrelated windows between them. Windows
    /// with a remembered position come first, in that order; anything homed here without one
    /// follows, in id order for determinism.
    pub fn windows_homed_to(&self, display: &str) -> Vec<WindowId> {
        let mut homed: Vec<WindowId> = self
            .window_home
            .iter()
            .filter_map(|(window, home)| (home == display).then_some(*window))
            .collect();
        homed.sort_unstable();

        let mut ordered: Vec<WindowId> = Vec::with_capacity(homed.len());
        if let Some(order) = self.display_strip.get(display) {
            ordered.extend(order.iter().copied().filter(|window| homed.contains(window)));
        }
        let remainder: Vec<WindowId> =
            homed.into_iter().filter(|window| !ordered.contains(window)).collect();
        ordered.extend(remainder);
        ordered
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn win(idx: u32) -> WindowId {
        WindowId::new(1, idx)
    }

    /// The order macOS lists screens in is not stable, and two names for one arrangement would make
    /// a replug pick between two remembered layouts at random.
    #[test]
    fn an_arrangement_has_one_name_whatever_order_the_displays_arrive_in() {
        assert_eq!(SetupId::of(["a", "b"]), SetupId::of(["b", "a"]));
    }

    #[test]
    fn a_repeated_display_does_not_change_the_name() {
        assert_eq!(SetupId::of(["a", "a", "b"]), SetupId::of(["a", "b"]));
    }

    /// The three arrangements the user actually has: laptop alone, laptop plus external, external
    /// alone with the lid shut. All different, and none is a prefix-match for another.
    #[test]
    fn adding_or_removing_a_display_is_a_different_arrangement() {
        let laptop = SetupId::of(["builtin"]);
        let docked = SetupId::of(["builtin", "studio"]);
        let lid_shut = SetupId::of(["studio"]);
        assert_ne!(laptop, docked);
        assert_ne!(docked, lid_shut);
        assert_ne!(laptop, lid_shut);
    }

    /// An external at home and one at the office are different arrangements, so the office layout
    /// cannot be applied to the home monitor.
    #[test]
    fn two_different_externals_are_two_arrangements() {
        assert_ne!(
            SetupId::of(["builtin", "home-4k"]),
            SetupId::of(["builtin", "office-4k"])
        );
    }

    /// macOS reports an empty screen list mid-reconfiguration, and a screen whose UUID has not
    /// arrived yet reports an empty one. Neither may name an arrangement.
    #[test]
    fn a_nameless_display_is_not_part_of_the_arrangement() {
        assert_eq!(SetupId::of(["builtin", ""]), SetupId::of(["builtin"]));
        assert!(SetupId::of([""]).is_empty());
        assert!(SetupId::of([]).is_empty());
    }

    #[test]
    fn an_arrangement_that_remembers_nothing_says_so() {
        let mut setup = Setup::default();
        assert!(setup.is_empty());
        setup.window_home.insert(win(1), "builtin".to_owned());
        assert!(!setup.is_empty());
    }

    #[test]
    fn strip_order_drives_the_order_windows_come_back_in() {
        let mut setup = Setup::default();
        for idx in [3, 1, 2] {
            setup.window_home.insert(win(idx), "studio".to_owned());
        }
        setup.display_strip.insert("studio".to_owned(), vec![win(3), win(1)]);

        // The remembered pair first, in that order; the one with no recorded place follows.
        assert_eq!(setup.windows_homed_to("studio"), vec![win(3), win(1), win(2)]);
    }

    #[test]
    fn forgetting_a_window_clears_every_record_of_it() {
        let mut setup = Setup::default();
        setup.window_home.insert(win(1), "studio".to_owned());
        setup.display_strip.insert("studio".to_owned(), vec![win(1), win(2)]);
        setup
            .window_width
            .entry("studio".to_owned())
            .or_default()
            .insert(win(1), ColumnWidth::FullWidth);

        setup.forget_window(win(1));

        assert!(setup.window_home.is_empty());
        assert_eq!(setup.display_strip["studio"], vec![win(2)]);
        assert!(setup.window_width.is_empty(), "an emptied map is dropped");
    }

    #[test]
    fn a_relaunched_window_carries_its_records_to_its_new_id() {
        let mut setup = Setup::default();
        setup.window_home.insert(win(1), "studio".to_owned());
        setup.display_strip.insert("studio".to_owned(), vec![win(1)]);
        setup
            .window_width
            .entry("studio".to_owned())
            .or_default()
            .insert(win(1), ColumnWidth::Offset(0.25));

        setup.rekey_window(win(1), win(9));

        assert_eq!(setup.window_home[&win(9)], "studio");
        assert_eq!(setup.display_strip["studio"], vec![win(9)]);
        assert_eq!(setup.window_width["studio"][&win(9)], ColumnWidth::Offset(0.25));
    }
}
