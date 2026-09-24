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
//!
//! Every window record is held per ARRANGEMENT — see [`crate::workspaces::domain::display_setup`].
//! This type owns the arrangements, remembers which one is in force, and answers every question
//! about it, so nothing above has to carry a [`SetupId`] around. The space mapping is the exception:
//! which display owns which native space is a fact about this session's hardware, not about one
//! arrangement, and it is retained for unplugged displays too.

use serde::{Deserialize, Serialize};

use rini_core::ids::SpaceId;
use rini_core::ids::{WindowId, pid_t};
use rustc_hash::FxHashMap as HashMap;

use crate::workspaces::domain::display_setup::{Setup, SetupId};

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct DisplayAffinity {
    /// Native space each display owns, by display UUID.
    ///
    /// Retained for every display seen in the session, including ones currently
    /// unplugged: that retained entry is the only thing that lets a replug find the
    /// layout it had before. Pruning it on unplug is what made reconnected displays
    /// come back empty.
    display_space: HashMap<String, SpaceId>,
    /// What each hardware arrangement remembers about the windows.
    #[serde(default)]
    setups: HashMap<SetupId, Setup>,
    /// The arrangement in force. Every window record is read and written under it.
    ///
    /// Persisted, so a restart into the same hardware reads the same arrangement rather than
    /// starting from nothing and re-homing everything.
    #[serde(default)]
    current: SetupId,
    /// Window records from a file written before arrangements existed, under the names that file
    /// used. Read, never written: the first arrangement to be named adopts them, so an upgrade keeps
    /// the homes it had instead of re-deriving every one of them.
    #[serde(default, skip_serializing, rename = "window_home")]
    legacy_window_home: HashMap<WindowId, String>,
    #[serde(default, skip_serializing, rename = "display_strip")]
    legacy_display_strip: HashMap<String, Vec<WindowId>>,
    #[serde(default, skip_serializing, rename = "window_width")]
    legacy_window_width: HashMap<String, HashMap<WindowId, ColumnWidth>>,
}

/// The width a window occupied, as the layout means it rather than in points.
///
/// Points would not survive a resolution change or a move between displays of different
/// widths, which is exactly when this record is consulted.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum ColumnWidth {
    /// Full viewport width, as `ctrl-F` and the widest preset both produce. Kept distinct
    /// from `Offset` because it is a MODE, not a ratio: it must stay full width on a
    /// display of any size, and it round-trips back to the preset width when toggled off.
    FullWidth,
    /// A deliberate width, as an offset from the configured `column_width_ratio`.
    Offset(f64),
}

/// What naming an arrangement turned out to mean.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetupChange {
    /// Already in force with records. Nothing to do.
    Unchanged,
    /// Seen before: its records apply, so windows can be put back where they were.
    Known,
    /// Never seen. It remembers nothing, so NOTHING may be moved — the windows stay where they are
    /// and this arrangement learns from where they end up.
    New,
    /// New, and it adopted the records of a file written before arrangements existed.
    Adopted,
    /// No displays named. macOS reports that mid-reconfiguration and it is never a real arrangement.
    Refused,
}

impl SetupChange {
    /// Whether the caller may put windows back. False for an arrangement with nothing to say, which
    /// is what stops a newly attached display from claiming windows it has never held.
    pub fn restores_windows(self) -> bool {
        matches!(self, Self::Known | Self::Adopted)
    }
}

impl DisplayAffinity {
    /// Nothing has been recorded. Used to tell a schema-5 file's nested section from a schema-4
    /// file's top-level one: a file has one shape or the other, never both.
    pub fn is_empty(&self) -> bool {
        self.display_space.is_empty()
            && self.setups.is_empty()
            && self.legacy_window_home.is_empty()
            && self.legacy_display_strip.is_empty()
            && self.legacy_window_width.is_empty()
    }

    /// Put the arrangement `displays` form in force, and say whether it is one nobody has seen.
    ///
    /// A new arrangement starts EMPTY and the caller must move nothing: a display nobody has used
    /// before has no claim on any window, so attaching a television in a meeting room leaves every
    /// window where it is and waits to be told. The windows then get homed where they already are by
    /// the ordinary settled-topology pass, and from then on this arrangement remembers itself.
    ///
    /// An arrangement of no displays is refused outright — macOS reports an empty screen list
    /// mid-reconfiguration, and switching to it would hand out the empty arrangement's records and
    /// then adopt the evacuation as the layout.
    pub fn use_setup(&mut self, displays: impl IntoIterator<Item = String>) -> SetupChange {
        let owned: Vec<String> = displays.into_iter().collect();
        let id = SetupId::of(owned.iter().map(String::as_str));
        if id.is_empty() {
            return SetupChange::Refused;
        }
        if id == self.current && self.setups.contains_key(&id) {
            return SetupChange::Unchanged;
        }

        let legacy = self.take_legacy();
        let entry = self.setups.entry(id.clone());
        let known = matches!(entry, std::collections::hash_map::Entry::Occupied(_));
        let setup = entry.or_default();
        let mut adopted = false;
        if !known && setup.is_empty() && !legacy.is_empty() {
            *setup = legacy;
            adopted = true;
        }
        self.current = id;
        match (known, adopted) {
            (true, _) => SetupChange::Known,
            (false, true) => SetupChange::Adopted,
            (false, false) => SetupChange::New,
        }
    }

    /// The arrangement in force.
    pub fn current_setup(&self) -> &SetupId {
        &self.current
    }

    /// Records written before any arrangement was named, ready to be adopted by the first one.
    ///
    /// Two sources, both meaning "we did not know the arrangement yet": the field names a schema-5
    /// file used, and the unnamed arrangement that writes land in before the first settled topology.
    /// Windows are seen before the screens settle, so without this their homes were recorded under a
    /// nameless arrangement and then never read again.
    fn take_legacy(&mut self) -> Setup {
        let mut legacy = Setup {
            window_home: std::mem::take(&mut self.legacy_window_home),
            display_strip: std::mem::take(&mut self.legacy_display_strip),
            window_width: std::mem::take(&mut self.legacy_window_width),
        };
        if let Some(unnamed) = self.setups.remove(&SetupId::default()) {
            // The file's records lose to what this session has actually observed.
            legacy.window_home.extend(unnamed.window_home);
            legacy.display_strip.extend(unnamed.display_strip);
            for (display, widths) in unnamed.window_width {
                legacy.window_width.entry(display).or_default().extend(widths);
            }
        }
        legacy
    }

    /// The records of the arrangement in force. Empty when none has been named yet, which answers
    /// every query with "nothing remembered" rather than panicking.
    fn setup(&self) -> &Setup {
        static EMPTY: std::sync::LazyLock<Setup> = std::sync::LazyLock::new(Setup::default);
        self.setups.get(&self.current).unwrap_or(&EMPTY)
    }

    fn setup_mut(&mut self) -> &mut Setup {
        self.setups.entry(self.current.clone()).or_default()
    }

    /// Whether the arrangement in force remembers anything about the windows.
    pub fn current_setup_is_empty(&self) -> bool {
        self.setup().is_empty()
    }
    /// Record that `display` currently owns `space`, evicting any other claimant: a native space
    /// has one display, and two claimants make the affinity pass move windows forever.
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
        self.setup_mut().window_home.insert(window, display.to_owned());
    }

    /// Record a home only if the window does not already have one.
    ///
    /// Used at first sighting. A window that has been seen before keeps the display it
    /// was last deliberately placed on, even when it is currently parked elsewhere.
    pub fn set_window_home_if_absent(&mut self, window: WindowId, display: &str) {
        self.setup_mut()
            .window_home
            .entry(window.to_owned())
            .or_insert_with(|| display.to_owned());
    }

    pub fn window_home(&self, window: WindowId) -> Option<&str> {
        self.setup().window_home.get(&window).map(String::as_str)
    }

    /// Windows homed to `display` in the arrangement in force, in the strip order last observed.
    pub fn windows_homed_to(&self, display: &str) -> Vec<WindowId> {
        self.setup().windows_homed_to(display)
    }

    /// Remember the strip order currently on `display`.
    ///
    /// Recorded continuously while the display is attached, so the last snapshot before an
    /// unplug is the arrangement the user actually left behind — including windows opened,
    /// dragged in, or reshuffled since they were first homed.
    pub fn set_display_strip(&mut self, display: &str, windows: Vec<WindowId>) {
        if windows.is_empty() {
            self.setup_mut().display_strip.remove(display);
        } else {
            self.setup_mut().display_strip.insert(display.to_owned(), windows);
        }
    }

    pub fn display_strip(&self, display: &str) -> &[WindowId] {
        self.setup().display_strip.get(display).map(Vec::as_slice).unwrap_or_default()
    }

    /// Remember the width `window` occupies on `display`.
    pub fn set_window_width(&mut self, display: &str, window: WindowId, width: ColumnWidth) {
        self.setup_mut()
            .window_width
            .entry(display.to_owned())
            .or_default()
            .insert(window, width);
    }

    /// Forget any remembered width, so the window adopts the display's default.
    ///
    /// Distinct from never having had one: toggling a deliberate width back off is an
    /// instruction to stop pinning it, not to keep the old value.
    pub fn clear_window_width(&mut self, display: &str, window: WindowId) {
        let setup = self.setup_mut();
        if let Some(widths) = setup.window_width.get_mut(display) {
            widths.remove(&window);
            if widths.is_empty() {
                setup.window_width.remove(display);
            }
        }
    }

    /// The width `window` last had on `display`, if it ever had a deliberate one.
    pub fn window_width(&self, display: &str, window: WindowId) -> Option<ColumnWidth> {
        self.setup().window_width.get(display)?.get(&window).copied()
    }

    /// Every window that currently has a home, in any display.
    pub fn homed_windows(&self) -> Vec<WindowId> {
        let mut windows: Vec<WindowId> = self.setup().window_home.keys().copied().collect();
        windows.sort_unstable();
        windows
    }

    /// Forget a window in EVERY arrangement, not only the one in force.
    ///
    /// A `WindowId` dies with its window, so a record of it under another arrangement is a record
    /// that can never match again — and the arrangement it belongs to is not attached to be cleaned
    /// up later. Leaving them is how the lists filled with closed windows while an external was
    /// unplugged, which made repatriation report homes for three dead windows and bring nothing back.
    pub fn forget_window(&mut self, window: WindowId) {
        for setup in self.setups.values_mut() {
            setup.forget_window(window);
        }
        self.legacy_window_home.remove(&window);
    }

    pub fn forget_app(&mut self, pid: pid_t) {
        for setup in self.setups.values_mut() {
            setup.forget_app(pid);
        }
        self.legacy_window_home.retain(|window, _| window.pid != pid);
    }

    /// Carry a window's records across an identity change (an app relaunching into a new
    /// `WindowId` for the same window), in every arrangement: the window is the same window under
    /// all of them, and the arrangements not in force are exactly the ones nothing else will fix.
    pub fn rekey_window(&mut self, from: WindowId, to: WindowId) {
        for setup in self.setups.values_mut() {
            setup.rekey_window(from, to);
        }
        if let Some(home) = self.legacy_window_home.remove(&from) {
            self.legacy_window_home.insert(to, home);
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
        self.setup().window_home.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn win(idx: u32) -> WindowId {
        WindowId::new(1, idx)
    }

    fn docked() -> Vec<String> {
        vec!["builtin".to_owned(), "studio".to_owned()]
    }

    fn laptop() -> Vec<String> {
        vec!["builtin".to_owned()]
    }

    /// The reported requirement. A window arranged one way while docked must not disturb how the
    /// laptop looks alone, and coming back must find the docked arrangement intact.
    #[test]
    fn each_arrangement_remembers_its_own_layout() {
        let mut affinity = DisplayAffinity::default();

        affinity.use_setup(docked());
        affinity.set_window_home(win(1), "studio");
        affinity.set_window_width("studio", win(1), ColumnWidth::Offset(0.0));

        // Undocked: the same window is on the laptop, full width. Nothing about the docked
        // arrangement may change.
        affinity.use_setup(laptop());
        assert_eq!(
            affinity.window_home(win(1)),
            None,
            "the laptop arrangement has not been told anything about this window yet"
        );
        affinity.set_window_home(win(1), "builtin");
        affinity.set_window_width("builtin", win(1), ColumnWidth::FullWidth);

        affinity.use_setup(docked());
        assert_eq!(affinity.window_home(win(1)), Some("studio"));
        assert_eq!(
            affinity.window_width("studio", win(1)),
            Some(ColumnWidth::Offset(0.0))
        );

        affinity.use_setup(laptop());
        assert_eq!(affinity.window_home(win(1)), Some("builtin"));
        assert_eq!(
            affinity.window_width("builtin", win(1)),
            Some(ColumnWidth::FullWidth),
            "full width on the laptop alone, half when docked: the case one key per display cannot express"
        );
    }

    /// The television case. A display nobody has used before must not be handed a layout, and the
    /// caller is told so, because moving windows onto it is exactly what must not happen.
    #[test]
    fn an_arrangement_nobody_has_seen_remembers_nothing_and_says_so() {
        let mut affinity = DisplayAffinity::default();
        affinity.use_setup(laptop());
        affinity.set_window_home(win(1), "builtin");

        let change = affinity.use_setup(vec!["builtin".to_owned(), "projector".to_owned()]);
        assert_eq!(change, SetupChange::New);
        assert!(
            !change.restores_windows(),
            "nothing may be moved onto a display that has never held anything"
        );
        assert_eq!(
            affinity.window_home(win(1)),
            None,
            "and it must not inherit the laptop's arrangement either"
        );
    }

    #[test]
    fn returning_to_a_known_arrangement_restores_windows() {
        let mut affinity = DisplayAffinity::default();
        affinity.use_setup(docked());
        affinity.set_window_home(win(1), "studio");

        affinity.use_setup(laptop());
        let change = affinity.use_setup(docked());
        assert_eq!(change, SetupChange::Known);
        assert!(change.restores_windows());
        assert_eq!(affinity.windows_homed_to("studio"), vec![win(1)]);
    }

    /// macOS reports no screens at all mid-reconfiguration. Taking that as an arrangement would hand
    /// out an empty layout and then record the evacuation as the truth.
    #[test]
    fn an_arrangement_of_no_displays_is_refused() {
        let mut affinity = DisplayAffinity::default();
        affinity.use_setup(docked());
        affinity.set_window_home(win(1), "studio");

        assert_eq!(affinity.use_setup(Vec::new()), SetupChange::Refused);
        assert_eq!(
            affinity.window_home(win(1)),
            Some("studio"),
            "the arrangement in force is untouched"
        );
    }

    /// Windows are seen before the screens settle, so their homes are written before any arrangement
    /// is named. The first arrangement to be named adopts them rather than starting blank.
    #[test]
    fn records_written_before_the_screens_settled_are_adopted() {
        let mut affinity = DisplayAffinity::default();
        affinity.set_window_home(win(1), "builtin");
        affinity.set_window_width("builtin", win(1), ColumnWidth::FullWidth);

        let change = affinity.use_setup(laptop());
        assert_eq!(change, SetupChange::Adopted);
        assert!(change.restores_windows());
        assert_eq!(affinity.window_home(win(1)), Some("builtin"));
        assert_eq!(
            affinity.window_width("builtin", win(1)),
            Some(ColumnWidth::FullWidth)
        );
    }

    /// Only the FIRST arrangement adopts them. A second one is a genuinely new arrangement and must
    /// start empty, or every arrangement would inherit the same layout forever.
    #[test]
    fn only_the_first_arrangement_adopts_the_unnamed_records() {
        let mut affinity = DisplayAffinity::default();
        affinity.set_window_home(win(1), "builtin");
        affinity.use_setup(laptop());

        assert_eq!(affinity.use_setup(docked()), SetupChange::New);
        assert_eq!(affinity.window_home(win(1)), None);
    }

    /// A dead window's id can never match again, and the arrangement it is recorded under may not be
    /// attached for months. Leaving the record is how the lists filled with closed windows.
    #[test]
    fn forgetting_a_window_reaches_every_arrangement() {
        let mut affinity = DisplayAffinity::default();
        affinity.use_setup(docked());
        affinity.set_window_home(win(1), "studio");
        affinity.use_setup(laptop());
        affinity.set_window_home(win(1), "builtin");

        affinity.forget_window(win(1));

        assert_eq!(affinity.window_home(win(1)), None);
        affinity.use_setup(docked());
        assert_eq!(
            affinity.window_home(win(1)),
            None,
            "gone from the arrangement that was not in force too"
        );
    }

    /// The same window under a new id after a relaunch, in every arrangement for the same reason.
    #[test]
    fn a_relaunch_rekeys_every_arrangement() {
        let mut affinity = DisplayAffinity::default();
        affinity.use_setup(docked());
        affinity.set_window_home(win(1), "studio");
        affinity.use_setup(laptop());
        affinity.set_window_home(win(1), "builtin");

        affinity.rekey_window(win(1), win(7));

        assert_eq!(affinity.window_home(win(7)), Some("builtin"));
        affinity.use_setup(docked());
        assert_eq!(affinity.window_home(win(7)), Some("studio"));
    }

    /// Naming the arrangement already in force changes nothing, so a settled topology that reports
    /// the same screens does not look like a change and trigger a needless pass.
    #[test]
    fn naming_the_arrangement_in_force_is_unchanged() {
        let mut affinity = DisplayAffinity::default();
        affinity.use_setup(docked());
        assert_eq!(affinity.use_setup(docked()), SetupChange::Unchanged);
        assert_eq!(
            affinity.use_setup(vec!["studio".to_owned(), "builtin".to_owned()]),
            SetupChange::Unchanged,
            "and the order the screens are reported in is not a change"
        );
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

#[cfg(test)]
mod observation_tests {
    use super::*;

    const BUILT_IN: &str = "37D8832A-2D66-02CA-B9F7-8F30A301B230";
    const EXTERNAL: &str = "B5E7ECDB-94D9-4565-949D-5F22F78D104A";

    fn win(idx: u32) -> WindowId {
        WindowId::new(500, idx)
    }

    /// The bug behind "windows teleport between displays". A window homed to the built-in is SEEN on
    /// the external — because rini parked it there, or evacuated it, or laid it out there — and the
    /// settled-topology pass used to make that its new home. It then never came back on a replug.
    ///
    /// Every real way a window changes display writes the home at the moment the user asks: a drag,
    /// an explicit move, a first sighting, a restore. An observation is not one of those; it is the
    /// result of rini's own layout, which already follows from the home.
    #[test]
    fn seeing_a_window_on_another_display_does_not_re_home_it() {
        let mut affinity = DisplayAffinity::default();
        affinity.set_window_home(win(1), BUILT_IN);

        affinity.set_window_home_if_absent(win(1), EXTERNAL);

        assert_eq!(
            affinity.window_home(win(1)),
            Some(BUILT_IN),
            "an observation must not overwrite a home the user chose"
        );
    }

    /// The pass still has to home a window nobody has placed yet, which is the first sighting and
    /// the reason it runs at all.
    #[test]
    fn a_window_with_no_home_takes_the_display_it_is_seen_on() {
        let mut affinity = DisplayAffinity::default();
        affinity.set_window_home_if_absent(win(2), EXTERNAL);
        assert_eq!(affinity.window_home(win(2)), Some(EXTERNAL));
    }

    /// An explicit intent still moves it. This is what a drag and a move command call.
    #[test]
    fn an_explicit_move_re_homes_the_window() {
        let mut affinity = DisplayAffinity::default();
        affinity.set_window_home(win(3), BUILT_IN);
        affinity.set_window_home(win(3), EXTERNAL);
        assert_eq!(affinity.window_home(win(3)), Some(EXTERNAL));
    }
}
