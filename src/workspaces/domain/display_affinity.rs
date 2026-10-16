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

use rini_core::ids::{DisplayUuid, SpaceId, WindowId, pid_t};
use rustc_hash::{FxHashMap as HashMap, FxHashSet as HashSet};
use serde::{Deserialize, Serialize};

use crate::layout::ColumnWidth;
use crate::workspaces::domain::display_setup::{Setup, SetupId};

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct DisplayAffinity {
    /// Native space each display owns, by display UUID.
    ///
    /// Retained for every display seen in the session, including ones currently
    /// unplugged: that retained entry is the only thing that lets a replug find the
    /// layout it had before. Pruning it on unplug is what made reconnected displays
    /// come back empty.
    display_space: HashMap<DisplayUuid, SpaceId>,
    /// What each hardware arrangement remembers about the windows.
    #[serde(default)]
    setups: HashMap<SetupId, Setup>,
    /// The arrangement in force. Every window record is read and written under it.
    ///
    /// Persisted, so a restart into the same hardware reads the same arrangement rather than
    /// starting from nothing and re-homing everything.
    #[serde(default)]
    current: SetupId,
}

/// A `ColumnWidth` as a file holds it: the two variants every rini reads. An unbounded width is
/// held as its offset, and the mark beside it in a field an older rini skips, so going back to one
/// keeps every width rather than refusing the file and the machine's memory with it.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum StoredWidth {
    FullWidth,
    Offset(f64),
}

impl StoredWidth {
    /// `width` as a file holds it, and whether it is unbounded.
    pub fn of(width: ColumnWidth) -> (Self, bool) {
        match width {
            ColumnWidth::FullWidth => (Self::FullWidth, false),
            ColumnWidth::Offset(offset) => (Self::Offset(offset), false),
            ColumnWidth::Unbounded(offset) => (Self::Offset(offset), true),
        }
    }

    /// The width it holds, given whether the file marks it unbounded.
    pub fn read(self, unbounded: bool) -> ColumnWidth {
        match self {
            Self::FullWidth => ColumnWidth::FullWidth,
            Self::Offset(offset) if unbounded => ColumnWidth::Unbounded(offset),
            Self::Offset(offset) => ColumnWidth::Offset(offset),
        }
    }
}

/// Widths per display and window, as a file holds them.
pub type StoredWidths = HashMap<DisplayUuid, HashMap<WindowId, StoredWidth>>;

/// The windows on each display whose stored width is unbounded.
pub type UnboundedWidths = HashMap<DisplayUuid, HashSet<WindowId>>;

/// `widths` as a file holds them.
pub fn store_widths(
    widths: &HashMap<DisplayUuid, HashMap<WindowId, ColumnWidth>>,
) -> (StoredWidths, UnboundedWidths) {
    let mut stored = StoredWidths::default();
    let mut unbounded = UnboundedWidths::default();
    for (display, windows) in widths {
        for (&window, &width) in windows {
            let (width, is_unbounded) = StoredWidth::of(width);
            stored.entry(display.clone()).or_default().insert(window, width);
            if is_unbounded {
                unbounded.entry(display.clone()).or_default().insert(window);
            }
        }
    }
    (stored, unbounded)
}

/// Widths read from a file.
pub fn read_widths(
    stored: StoredWidths,
    unbounded: &UnboundedWidths,
) -> HashMap<DisplayUuid, HashMap<WindowId, ColumnWidth>> {
    stored
        .into_iter()
        .map(|(display, windows)| {
            let marked = unbounded.get(&display);
            let windows = windows
                .into_iter()
                .map(|(window, width)| {
                    let unbounded = marked.is_some_and(|set| set.contains(&window));
                    (window, width.read(unbounded))
                })
                .collect();
            (display, windows)
        })
        .collect()
}

/// What naming an arrangement turned out to mean.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetupChange {
    /// Already in force, with records. They apply: an unplug and replug quick enough that nothing
    /// else was named in between still restores, which is why this counts as restorable rather than
    /// as "nothing to do".
    Unchanged,
    /// Seen before: its records apply, so windows can be put back where they were.
    Known,
    /// Never seen. It remembers nothing, so NOTHING may be moved — the windows stay where they are
    /// and this arrangement learns from where they end up.
    New,
    /// New, and it adopted the records written before any arrangement was named.
    Adopted,
    /// No displays named. macOS reports that mid-reconfiguration and it is never a real arrangement.
    Refused,
}

impl SetupChange {
    /// Whether the caller may put windows back. False only for an arrangement with nothing to say,
    /// which is what stops a newly attached display from claiming windows it has never held.
    pub fn restores_windows(self) -> bool {
        matches!(self, Self::Known | Self::Adopted | Self::Unchanged)
    }
}

impl DisplayAffinity {
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
    pub fn use_setup(&mut self, displays: impl IntoIterator<Item = DisplayUuid>) -> SetupChange {
        let owned: Vec<DisplayUuid> = displays.into_iter().collect();
        let id = SetupId::of(&owned);
        if id.is_empty() {
            return SetupChange::Refused;
        }
        if id == self.current && self.setups.contains_key(&id) {
            return SetupChange::Unchanged;
        }

        let unnamed = self.take_unnamed();
        let entry = self.setups.entry(id.clone());
        let known = matches!(entry, std::collections::hash_map::Entry::Occupied(_));
        let setup = entry.or_default();
        let mut adopted = false;
        if !known && setup.is_empty() && !unnamed.is_empty() {
            *setup = unnamed;
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
    pub fn current_setup(&self) -> &SetupId { &self.current }

    /// Records written before any arrangement was named, ready to be adopted by the first one.
    ///
    /// Windows are seen before the screens settle, so their homes land in the unnamed arrangement;
    /// without this they were recorded there and then never read again.
    fn take_unnamed(&mut self) -> Setup {
        self.setups.remove(&SetupId::default()).unwrap_or_default()
    }

    /// The records of the arrangement in force. Empty when none has been named yet, which answers
    /// every query with "nothing remembered" rather than panicking.
    fn setup(&self) -> &Setup {
        static EMPTY: std::sync::LazyLock<Setup> = std::sync::LazyLock::new(Setup::default);
        self.setups.get(&self.current).unwrap_or(&EMPTY)
    }

    fn setup_mut(&mut self) -> &mut Setup { self.setups.entry(self.current.clone()).or_default() }

    /// Record that `display` currently owns `space`, evicting any other claimant: a native space
    /// has one display, and two claimants make the affinity pass move windows forever.
    pub fn set_display_space(&mut self, display: &DisplayUuid, space: SpaceId) {
        self.display_space.retain(|uuid, owned| *owned != space || uuid == display);
        self.display_space.insert(display.clone(), space);
    }

    pub fn space_for_display(&self, display: &DisplayUuid) -> Option<SpaceId> {
        self.display_space.get(display).copied()
    }

    pub fn display_for_space(&self, space: SpaceId) -> Option<&DisplayUuid> {
        self.display_space
            .iter()
            .find_map(|(uuid, owned)| (*owned == space).then_some(uuid))
    }

    /// Move every record naming `old_space` onto `new_space`.
    pub fn remap_space(&mut self, old_space: SpaceId, new_space: SpaceId) {
        if old_space == new_space {
            return;
        }
        // The display arriving on new_space wins over whatever claimed it before, for
        // the same one-display-per-space reason as set_display_space.
        let arriving: Vec<DisplayUuid> = self
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
    pub fn set_window_home(&mut self, window: WindowId, display: &DisplayUuid) {
        let setup = self.setup_mut();
        setup.window_home.insert(window, display.clone());
        // The user chose this, so a display pin must not talk them out of it later.
        setup.home_by_intent.insert(window);
    }

    /// Give `window` the home a display pin asks for, unless the user has chosen one.
    ///
    /// Stronger than `set_window_home_if_absent`, because a home rini merely INFERRED from where the
    /// window happened to open is exactly what a pin exists to correct: an app that opens on the
    /// external is the reported case. Weaker than an explicit move, which is recorded as chosen and
    /// left alone.
    ///
    /// Not recorded as chosen itself: the pin is re-applied on every settled topology, so it does not
    /// need remembering, and marking it would make it indistinguishable from a real move.
    pub fn set_pinned_home(&mut self, window: WindowId, display: &DisplayUuid) -> bool {
        let setup = self.setup_mut();
        if setup.home_was_chosen(window) {
            return false;
        }
        let already = setup.window_home.get(&window) == Some(display);
        if already {
            return false;
        }
        setup.window_home.insert(window, display.clone());
        true
    }

    /// Record a home only if the window does not already have one.
    ///
    /// Used at first sighting. A window that has been seen before keeps the display it
    /// was last deliberately placed on, even when it is currently parked elsewhere.
    pub fn set_window_home_if_absent(&mut self, window: WindowId, display: &DisplayUuid) {
        self.setup_mut()
            .window_home
            .entry(window.to_owned())
            .or_insert_with(|| display.clone());
    }

    pub fn window_home(&self, window: WindowId) -> Option<&DisplayUuid> {
        self.setup().window_home.get(&window)
    }

    /// Windows homed to `display` in the arrangement in force, in the strip order last observed.
    pub fn windows_homed_to(&self, display: &DisplayUuid) -> Vec<WindowId> {
        self.setup().windows_homed_to(display)
    }

    /// Remember the strip order currently on `display`.
    ///
    /// Recorded continuously while the display is attached, so the last snapshot before an
    /// unplug is the arrangement the user actually left behind — including windows opened,
    /// dragged in, or reshuffled since they were first homed.
    pub fn set_display_strip(&mut self, display: &DisplayUuid, windows: Vec<WindowId>) {
        if windows.is_empty() {
            self.setup_mut().display_strip.remove(display);
        } else {
            self.setup_mut().display_strip.insert(display.clone(), windows);
        }
    }

    /// Remember the width `window` occupies on `display`.
    pub fn set_window_width(
        &mut self,
        display: &DisplayUuid,
        window: WindowId,
        width: ColumnWidth,
    ) {
        self.setup_mut()
            .window_width
            .entry(display.clone())
            .or_default()
            .insert(window, width);
    }

    /// Forget any remembered width, so the window adopts the display's default.
    ///
    /// Distinct from never having had one: toggling a deliberate width back off is an
    /// instruction to stop pinning it, not to keep the old value.
    pub fn clear_window_width(&mut self, display: &DisplayUuid, window: WindowId) {
        let setup = self.setup_mut();
        if let Some(widths) = setup.window_width.get_mut(display) {
            widths.remove(&window);
            if widths.is_empty() {
                setup.window_width.remove(display);
            }
        }
    }

    /// The width `window` last had on `display`, if it ever had a deliberate one.
    pub fn window_width(&self, display: &DisplayUuid, window: WindowId) -> Option<ColumnWidth> {
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
    }

    pub fn forget_app(&mut self, pid: pid_t) {
        for setup in self.setups.values_mut() {
            setup.forget_app(pid);
        }
    }

    /// Carry a window's records across an identity change (an app relaunching into a new
    /// `WindowId` for the same window), in every arrangement: the window is the same window under
    /// all of them, and the arrangements not in force are exactly the ones nothing else will fix.
    pub fn rekey_window(&mut self, from: WindowId, to: WindowId) {
        for setup in self.setups.values_mut() {
            setup.rekey_window(from, to);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn win(idx: u32) -> WindowId { WindowId::new(1, idx) }

    fn uuid(uuid: &str) -> DisplayUuid { DisplayUuid::new(uuid) }

    fn docked() -> Vec<DisplayUuid> { vec![uuid("builtin"), uuid("studio")] }

    fn laptop() -> Vec<DisplayUuid> { vec![uuid("builtin")] }

    /// The reported requirement. A window arranged one way while docked must not disturb how the
    /// laptop looks alone, and coming back must find the docked arrangement intact.
    #[test]
    fn each_arrangement_remembers_its_own_layout() {
        let mut affinity = DisplayAffinity::default();

        affinity.use_setup(docked());
        affinity.set_window_home(win(1), &uuid("studio"));
        affinity.set_window_width(&uuid("studio"), win(1), ColumnWidth::Offset(0.0));

        // Undocked: the same window is on the laptop, full width. Nothing about the docked
        // arrangement may change.
        affinity.use_setup(laptop());
        assert_eq!(
            affinity.window_home(win(1)),
            None,
            "the laptop arrangement has not been told anything about this window yet"
        );
        affinity.set_window_home(win(1), &uuid("builtin"));
        affinity.set_window_width(&uuid("builtin"), win(1), ColumnWidth::FullWidth);

        affinity.use_setup(docked());
        assert_eq!(affinity.window_home(win(1)), Some(&uuid("studio")));
        assert_eq!(
            affinity.window_width(&uuid("studio"), win(1)),
            Some(ColumnWidth::Offset(0.0))
        );

        affinity.use_setup(laptop());
        assert_eq!(affinity.window_home(win(1)), Some(&uuid("builtin")));
        assert_eq!(
            affinity.window_width(&uuid("builtin"), win(1)),
            Some(ColumnWidth::FullWidth),
            "full width on the laptop alone, half when docked: the case one key per display cannot express"
        );
    }

    /// A pin corrects a home rini merely inferred from where a window happened to open, which is the
    /// reported case: Slack opening on the external when it belongs on the laptop.
    #[test]
    fn a_pin_overrides_an_inferred_home() {
        let mut affinity = DisplayAffinity::default();
        affinity.use_setup(docked());
        affinity.set_window_home_if_absent(win(1), &uuid("studio"));

        assert!(affinity.set_pinned_home(win(1), &uuid("builtin")));
        assert_eq!(affinity.window_home(win(1)), Some(&uuid("builtin")));
    }

    /// And never overrides one the user chose. "Unless I explicitly move it there."
    #[test]
    fn a_pin_leaves_a_chosen_home_alone() {
        let mut affinity = DisplayAffinity::default();
        affinity.use_setup(docked());
        affinity.set_window_home(win(1), &uuid("studio"));

        assert!(!affinity.set_pinned_home(win(1), &uuid("builtin")));
        assert_eq!(
            affinity.window_home(win(1)),
            Some(&uuid("studio")),
            "the window stays where the user put it"
        );
    }

    /// Choosing is per arrangement, like everything else. Moving a pinned window to the external
    /// while docked must not disable the pin on the laptop alone.
    #[test]
    fn choosing_a_home_in_one_arrangement_does_not_free_the_pin_in_another() {
        let mut affinity = DisplayAffinity::default();
        affinity.use_setup(docked());
        affinity.set_window_home(win(1), &uuid("studio"));

        affinity.use_setup(laptop());
        affinity.set_window_home_if_absent(win(1), &uuid("studio"));
        assert!(
            affinity.set_pinned_home(win(1), &uuid("builtin")),
            "the laptop arrangement never heard the user choose anything"
        );
    }

    /// Re-applying the same pin is not a change, so the settled-topology pass does not report work it
    /// did not do on every single event.
    #[test]
    fn re_applying_a_pin_that_already_holds_changes_nothing() {
        let mut affinity = DisplayAffinity::default();
        affinity.use_setup(laptop());
        assert!(affinity.set_pinned_home(win(1), &uuid("builtin")));
        assert!(!affinity.set_pinned_home(win(1), &uuid("builtin")));
    }

    /// The television case. A display nobody has used before must not be handed a layout, and the
    /// caller is told so, because moving windows onto it is exactly what must not happen.
    #[test]
    fn an_arrangement_nobody_has_seen_remembers_nothing_and_says_so() {
        let mut affinity = DisplayAffinity::default();
        affinity.use_setup(laptop());
        affinity.set_window_home(win(1), &uuid("builtin"));

        let change = affinity.use_setup(vec![uuid("builtin"), uuid("projector")]);
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
        affinity.set_window_home(win(1), &uuid("studio"));

        affinity.use_setup(laptop());
        let change = affinity.use_setup(docked());
        assert_eq!(change, SetupChange::Known);
        assert!(change.restores_windows());
        assert_eq!(affinity.windows_homed_to(&uuid("studio")), vec![win(1)]);
    }

    /// macOS reports no screens at all mid-reconfiguration. Taking that as an arrangement would hand
    /// out an empty layout and then record the evacuation as the truth.
    #[test]
    fn an_arrangement_of_no_displays_is_refused() {
        let mut affinity = DisplayAffinity::default();
        affinity.use_setup(docked());
        affinity.set_window_home(win(1), &uuid("studio"));

        assert_eq!(affinity.use_setup(Vec::new()), SetupChange::Refused);
        assert_eq!(
            affinity.window_home(win(1)),
            Some(&uuid("studio")),
            "the arrangement in force is untouched"
        );
    }

    /// Windows are seen before the screens settle, so their homes are written before any arrangement
    /// is named. The first arrangement to be named adopts them rather than starting blank.
    #[test]
    fn records_written_before_the_screens_settled_are_adopted() {
        let mut affinity = DisplayAffinity::default();
        affinity.set_window_home(win(1), &uuid("builtin"));
        affinity.set_window_width(&uuid("builtin"), win(1), ColumnWidth::FullWidth);

        let change = affinity.use_setup(laptop());
        assert_eq!(change, SetupChange::Adopted);
        assert!(change.restores_windows());
        assert_eq!(affinity.window_home(win(1)), Some(&uuid("builtin")));
        assert_eq!(
            affinity.window_width(&uuid("builtin"), win(1)),
            Some(ColumnWidth::FullWidth)
        );
    }

    /// Only the FIRST arrangement adopts them. A second one is a genuinely new arrangement and must
    /// start empty, or every arrangement would inherit the same layout forever.
    #[test]
    fn only_the_first_arrangement_adopts_the_unnamed_records() {
        let mut affinity = DisplayAffinity::default();
        affinity.set_window_home(win(1), &uuid("builtin"));
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
        affinity.set_window_home(win(1), &uuid("studio"));
        affinity.use_setup(laptop());
        affinity.set_window_home(win(1), &uuid("builtin"));

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
        affinity.set_window_home(win(1), &uuid("studio"));
        affinity.use_setup(laptop());
        affinity.set_window_home(win(1), &uuid("builtin"));

        affinity.rekey_window(win(1), win(7));

        assert_eq!(affinity.window_home(win(7)), Some(&uuid("builtin")));
        affinity.use_setup(docked());
        assert_eq!(affinity.window_home(win(7)), Some(&uuid("studio")));
    }

    /// Naming the arrangement already in force changes nothing, so a settled topology that reports
    /// the same screens does not look like a change and trigger a needless pass.
    #[test]
    fn naming_the_arrangement_in_force_is_unchanged() {
        let mut affinity = DisplayAffinity::default();
        affinity.use_setup(docked());
        let change = affinity.use_setup(docked());
        assert_eq!(change, SetupChange::Unchanged);
        assert!(
            change.restores_windows(),
            "a quick unplug and replug with nothing named in between still restores"
        );
        assert_eq!(
            affinity.use_setup(vec![uuid("studio"), uuid("builtin")]),
            SetupChange::Unchanged,
            "and the order the screens are reported in is not a change"
        );
    }

    #[test]
    fn a_space_belongs_to_one_display_at_a_time() {
        let mut affinity = DisplayAffinity::default();
        affinity.set_display_space(&uuid("built-in"), SpaceId::new(1));
        // macOS handed space 1 to the external. The built-in must not still claim it.
        affinity.set_display_space(&uuid("external"), SpaceId::new(1));

        assert_eq!(
            affinity.space_for_display(&uuid("external")),
            Some(SpaceId::new(1))
        );
        assert_eq!(affinity.space_for_display(&uuid("built-in")), None);
        assert_eq!(
            affinity.display_for_space(SpaceId::new(1)),
            Some(&uuid("external"))
        );
    }

    #[test]
    fn reconnect_keeps_the_display_known_under_its_new_space_id() {
        let mut affinity = DisplayAffinity::default();
        affinity.set_display_space(&uuid("external"), SpaceId::new(479));
        affinity.remap_space(SpaceId::new(479), SpaceId::new(552));

        assert_eq!(
            affinity.space_for_display(&uuid("external")),
            Some(SpaceId::new(552))
        );
        assert_eq!(affinity.display_for_space(SpaceId::new(479)), None);
    }

    #[test]
    fn remap_does_not_leave_two_displays_on_the_target_space() {
        let mut affinity = DisplayAffinity::default();
        affinity.set_display_space(&uuid("external"), SpaceId::new(479));
        affinity.set_display_space(&uuid("built-in"), SpaceId::new(552));
        // The external comes back and macOS gives it the id the built-in was using.
        affinity.remap_space(SpaceId::new(479), SpaceId::new(552));

        assert_eq!(
            affinity.space_for_display(&uuid("external")),
            Some(SpaceId::new(552))
        );
        assert_eq!(affinity.space_for_display(&uuid("built-in")), None);
    }

    #[test]
    fn first_sighting_does_not_overwrite_a_deliberate_home() {
        let mut affinity = DisplayAffinity::default();
        affinity.set_window_home(win(1), &uuid("external"));
        // Unplug parks the window on the built-in and it is seen there again.
        affinity.set_window_home_if_absent(win(1), &uuid("built-in"));

        assert_eq!(affinity.window_home(win(1)), Some(&uuid("external")));
        assert_eq!(affinity.windows_homed_to(&uuid("external")), vec![win(1)]);
    }

    #[test]
    fn an_explicit_move_does_overwrite_the_home() {
        let mut affinity = DisplayAffinity::default();
        affinity.set_window_home(win(1), &uuid("external"));
        affinity.set_window_home(win(1), &uuid("built-in"));

        assert_eq!(affinity.window_home(win(1)), Some(&uuid("built-in")));
        assert!(affinity.windows_homed_to(&uuid("external")).is_empty());
    }

    #[test]
    fn a_window_seen_on_a_new_display_is_re_homed_to_it() {
        let mut affinity = DisplayAffinity::default();
        affinity.set_window_home(win(1), &uuid("external"));
        // Seen on the built-in while BOTH displays are attached: the user moved it, so the
        // built-in is its home now. Re-homing has to overwrite, not defer to the old value,
        // or a later replug of the external hauls the window back off the built-in.
        affinity.set_window_home(win(1), &uuid("built-in"));

        assert_eq!(affinity.window_home(win(1)), Some(&uuid("built-in")));
        assert!(affinity.windows_homed_to(&uuid("external")).is_empty());
    }

    #[test]
    fn strip_order_drives_repatriation_order() {
        let mut affinity = DisplayAffinity::default();
        // Ids deliberately out of visual order: sorting by WindowId would give 1, 2, 8, 9
        // and split the adjacent pair 8, 9 apart from where the user left them.
        for window in [win(1), win(8), win(9), win(2)] {
            affinity.set_window_home(window, &uuid("external"));
        }
        affinity.set_display_strip(&uuid("external"), vec![win(1), win(8), win(9), win(2)]);

        assert_eq!(affinity.windows_homed_to(&uuid("external")), vec![
            win(1),
            win(8),
            win(9),
            win(2)
        ]);
    }

    /// The point of keying width by display: one window, two displays, two answers.
    #[test]
    fn a_window_can_have_a_different_width_on_each_display() {
        let mut affinity = DisplayAffinity::default();
        affinity.set_window_width(&uuid("built-in"), win(1), ColumnWidth::FullWidth);
        affinity.set_window_width(&uuid("external"), win(1), ColumnWidth::Offset(0.25));

        assert_eq!(
            affinity.window_width(&uuid("built-in"), win(1)),
            Some(ColumnWidth::FullWidth)
        );
        assert_eq!(
            affinity.window_width(&uuid("external"), win(1)),
            Some(ColumnWidth::Offset(0.25))
        );
        // A display it has never been on has no opinion, so the window adopts that
        // display's configured default rather than inheriting another display's size.
        assert_eq!(affinity.window_width(&uuid("third"), win(1)), None);
    }

    /// A dragged width comes back unbounded, so no display's bounds clamp it after a restart. Every
    /// width is written as an `Offset` or `FullWidth`, as it was before a drag had a record, so an old
    /// file reads unchanged.
    #[test]
    fn a_dragged_width_round_trips_and_an_old_record_reads_as_before() {
        let mut affinity = DisplayAffinity::default();
        affinity.set_window_width(&uuid("built-in"), win(1), ColumnWidth::Unbounded(0.33797));
        affinity.set_window_width(&uuid("built-in"), win(2), ColumnWidth::Offset(0.25));

        let written = ron::ser::to_string(&affinity).unwrap();
        assert!(written.contains("Offset(0.25)"), "{written}");
        assert!(written.contains("Offset(0.33797)"), "{written}");
        assert!(!written.contains("Unbounded("), "{written}");
        let read: DisplayAffinity = ron::from_str(&written).unwrap();
        assert_eq!(
            read.window_width(&uuid("built-in"), win(1)),
            Some(ColumnWidth::Unbounded(0.33797))
        );
        assert_eq!(
            read.window_width(&uuid("built-in"), win(2)),
            Some(ColumnWidth::Offset(0.25))
        );
    }

    /// Clearing must forget, not freeze the last value: toggling a deliberate width off is an
    /// instruction to follow the display default again.
    #[test]
    fn clearing_a_width_restores_the_display_default() {
        let mut affinity = DisplayAffinity::default();
        affinity.set_window_width(&uuid("built-in"), win(1), ColumnWidth::FullWidth);
        affinity.clear_window_width(&uuid("built-in"), win(1));

        assert_eq!(affinity.window_width(&uuid("built-in"), win(1)), None);
    }

    #[test]
    fn forgetting_a_window_drops_its_remembered_widths() {
        let mut affinity = DisplayAffinity::default();
        affinity.set_window_width(&uuid("built-in"), win(1), ColumnWidth::FullWidth);
        affinity.set_window_width(&uuid("built-in"), win(2), ColumnWidth::Offset(0.1));
        affinity.forget_window(win(1));

        assert_eq!(affinity.window_width(&uuid("built-in"), win(1)), None);
        assert_eq!(
            affinity.window_width(&uuid("built-in"), win(2)),
            Some(ColumnWidth::Offset(0.1)),
            "forgetting one window must not disturb another"
        );
    }

    /// An app relaunching into a new WindowId must keep the size the user gave it, for the
    /// same reason its home display carries across.
    #[test]
    fn rekeying_carries_remembered_widths() {
        let mut affinity = DisplayAffinity::default();
        affinity.set_window_width(&uuid("built-in"), win(1), ColumnWidth::FullWidth);
        affinity.rekey_window(win(1), win(7));

        assert_eq!(affinity.window_width(&uuid("built-in"), win(1)), None);
        assert_eq!(
            affinity.window_width(&uuid("built-in"), win(7)),
            Some(ColumnWidth::FullWidth)
        );
    }

    #[test]
    fn forgetting_an_app_drops_widths_for_all_of_its_windows() {
        let mut affinity = DisplayAffinity::default();
        affinity.set_window_width(&uuid("built-in"), WindowId::new(1, 1), ColumnWidth::FullWidth);
        affinity.set_window_width(&uuid("built-in"), WindowId::new(1, 2), ColumnWidth::Offset(0.2));
        affinity.set_window_width(&uuid("built-in"), WindowId::new(2, 1), ColumnWidth::Offset(0.3));
        affinity.forget_app(1);

        assert_eq!(
            affinity.window_width(&uuid("built-in"), WindowId::new(1, 1)),
            None
        );
        assert_eq!(
            affinity.window_width(&uuid("built-in"), WindowId::new(1, 2)),
            None
        );
        assert_eq!(
            affinity.window_width(&uuid("built-in"), WindowId::new(2, 1)),
            Some(ColumnWidth::Offset(0.3)),
            "another app's windows must survive"
        );
    }

    #[test]
    fn a_window_homed_without_a_remembered_position_still_comes_back() {
        let mut affinity = DisplayAffinity::default();
        affinity.set_window_home(win(1), &uuid("external"));
        affinity.set_window_home(win(5), &uuid("external"));
        // Only one of them has a position; the other must not be silently dropped.
        affinity.set_display_strip(&uuid("external"), vec![win(5)]);

        assert_eq!(affinity.windows_homed_to(&uuid("external")), vec![
            win(5),
            win(1)
        ]);
    }

    #[test]
    fn forgetting_a_window_also_drops_it_from_every_strip() {
        let mut affinity = DisplayAffinity::default();
        affinity.set_window_home(win(1), &uuid("external"));
        affinity.set_display_strip(&uuid("external"), vec![win(1), win(2)]);
        affinity.forget_window(win(1));

        assert_eq!(affinity.setup().display_strip[&uuid("external")], vec![win(2)]);
        assert_eq!(affinity.window_home(win(1)), None);
    }
}

#[cfg(test)]
mod observation_tests {
    use super::*;

    const BUILT_IN: &str = "37D8832A-2D66-02CA-B9F7-8F30A301B230";
    const EXTERNAL: &str = "B5E7ECDB-94D9-4565-949D-5F22F78D104A";

    fn win(idx: u32) -> WindowId { WindowId::new(500, idx) }

    fn uuid(uuid: &str) -> DisplayUuid { DisplayUuid::new(uuid) }

    /// "Windows teleport between displays". A window homed to the built-in is SEEN on the external —
    /// because rini parked it there, or evacuated it, or laid it out there — and the settled-topology
    /// pass must not make that its new home, or it never comes back on a replug.
    ///
    /// Every real way a window changes display writes the home at the moment the user asks: a drag,
    /// an explicit move, a first sighting, a restore. An observation is not one of those; it is the
    /// result of rini's own layout, which already follows from the home.
    #[test]
    fn seeing_a_window_on_another_display_does_not_re_home_it() {
        let mut affinity = DisplayAffinity::default();
        affinity.set_window_home(win(1), &uuid(BUILT_IN));

        affinity.set_window_home_if_absent(win(1), &uuid(EXTERNAL));

        assert_eq!(
            affinity.window_home(win(1)),
            Some(&uuid(BUILT_IN)),
            "an observation must not overwrite a home the user chose"
        );
    }

    /// The pass still has to home a window nobody has placed yet, which is the first sighting and
    /// the reason it runs at all.
    #[test]
    fn a_window_with_no_home_takes_the_display_it_is_seen_on() {
        let mut affinity = DisplayAffinity::default();
        affinity.set_window_home_if_absent(win(2), &uuid(EXTERNAL));
        assert_eq!(affinity.window_home(win(2)), Some(&uuid(EXTERNAL)));
    }

    /// An explicit intent still moves it. This is what a drag and a move command call.
    #[test]
    fn an_explicit_move_re_homes_the_window() {
        let mut affinity = DisplayAffinity::default();
        affinity.set_window_home(win(3), &uuid(BUILT_IN));
        affinity.set_window_home(win(3), &uuid(EXTERNAL));
        assert_eq!(affinity.window_home(win(3)), Some(&uuid(EXTERNAL)));
    }
}
