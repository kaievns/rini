//! Which keys are held right now, from edge events plus the authoritative modifier mask.
//!
//! Two sources that do not agree, and the rules for reconciling them. A key-down/key-up pair is an
//! EDGE: it says what changed, and a tap that was disabled or a dropped event means an edge was
//! missed and the cache is now wrong. The modifier flags on every event are a LEVEL: they say what is
//! held right now, authoritatively, but only for modifiers.
//!
//! So modifiers are answered from the flags and everything else from the edge cache, and the cache is
//! discarded whenever the tap comes back. Getting this wrong means a binding that fires with no key
//! held, or one that never fires until the user presses and releases a modifier to resynchronise —
//! both of which look like rini ignoring the keyboard.

use rustc_hash::FxHashSet as HashSet;

use crate::input::domain::key::{KeyCode, is_modifier_key, modifier_key_is_active};

/// The keys currently held, as far as the tap can tell.
#[derive(Debug, Default)]
pub struct HeldKeys {
    /// Keys seen down and not yet up. Modifier keys are kept here too, but only as a hint about which
    /// SIDE was used; whether they are held at all comes from the flags.
    pressed: HashSet<KeyCode>,
    /// The modifier mask from the most recent event, as bits.
    flags: u64,
}

impl HeldKeys {
    /// The modifier mask last seen.
    pub fn flags(&self) -> u64 {
        self.flags
    }

    /// The keys seen down and not yet up.
    pub fn pressed(&self) -> &HashSet<KeyCode> {
        &self.pressed
    }

    pub fn key_down(&mut self, key: KeyCode) {
        self.pressed.insert(key);
    }

    pub fn key_up(&mut self, key: KeyCode) {
        self.pressed.remove(&key);
    }

    /// A modifier changed. The new mask is authoritative about whether it is held.
    ///
    /// Non-modifier keys never arrive this way and are ignored, because a flags-changed event for one
    /// would mean macOS had reclassified it and trusting that would drop a real key press.
    pub fn flags_changed(&mut self, flags: u64, key: KeyCode) {
        self.flags = flags;
        if !is_modifier_key(key) {
            return;
        }
        if modifier_key_is_active(flags, key) {
            self.pressed.insert(key);
        } else {
            self.pressed.remove(&key);
        }
    }

    /// Record the mask carried by an ordinary event, without touching the key set.
    pub fn observe_flags(&mut self, flags: u64) {
        self.flags = flags;
    }

    /// Drop every modifier the mask says is no longer held.
    ///
    /// Called when the flags and the cache may have drifted — a dropped event leaves a modifier stuck
    /// down, and a stuck modifier means every subsequent binding is evaluated as though it were held.
    /// Non-modifier keys are left alone: the flags say nothing about them, so removing them here would
    /// forget a key that really is down.
    pub fn reconcile_modifiers(&mut self) {
        let flags = self.flags;
        self.pressed.retain(|key| {
            if is_modifier_key(*key) {
                modifier_key_is_active(flags, *key)
            } else {
                true
            }
        });
    }

    /// Start again from the live mask, after the tap was re-enabled.
    ///
    /// Any key-up may have happened while the tap was off, so the whole edge cache is suspect and is
    /// thrown away rather than reconciled. The flags are a level and can be trusted; the cache cannot.
    pub fn tap_re_enabled(&mut self, flags: u64) {
        self.pressed.clear();
        self.flags = flags;
    }

    /// Whether this specific key is held.
    ///
    /// A modifier is answered from the flags, because that is the authoritative source and the cache
    /// may have missed an edge. Anything else is answered from the cache, because the flags say
    /// nothing about it.
    pub fn is_held(&self, key: KeyCode) -> bool {
        if is_modifier_key(key) {
            modifier_key_is_active(self.flags, key)
        } else {
            self.pressed.contains(&key)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::domain::key::MOD_FAMILIES;

    /// Ctrl's family mask and its two side bits, straight from the table.
    fn ctrl() -> &'static crate::input::domain::key::ModFamily {
        MOD_FAMILIES.iter().find(|f| f.name == "Ctrl").expect("Ctrl is a family")
    }

    #[test]
    fn an_ordinary_key_is_held_between_its_down_and_its_up() {
        let mut held = HeldKeys::default();
        assert!(!held.is_held(KeyCode::KeyA));

        held.key_down(KeyCode::KeyA);
        assert!(held.is_held(KeyCode::KeyA));

        held.key_up(KeyCode::KeyA);
        assert!(!held.is_held(KeyCode::KeyA));
    }

    /// A modifier is answered from the FLAGS, not the cache. The cache can have missed an edge; the
    /// flags arrive on every event and cannot.
    #[test]
    fn a_modifier_is_answered_from_the_flags_not_the_cache() {
        let mut held = HeldKeys::default();
        held.key_down(KeyCode::ControlLeft);
        assert!(
            !held.is_held(KeyCode::ControlLeft),
            "no flag bit set, so it is not held whatever the cache says"
        );

        held.observe_flags(ctrl().left_mask);
        assert!(held.is_held(KeyCode::ControlLeft));
    }

    /// The side bit is the only thing that tells the two apart. The family mask alone says "some Ctrl"
    /// and answering `ControlLeft` from it would fire a left-Ctrl binding on right Ctrl.
    #[test]
    fn the_family_mask_alone_does_not_make_either_side_held() {
        let mut held = HeldKeys::default();
        held.observe_flags(ctrl().mask);
        assert!(!held.is_held(KeyCode::ControlLeft));
        assert!(!held.is_held(KeyCode::ControlRight));
    }

    #[test]
    fn each_side_bit_holds_only_its_own_side() {
        let mut held = HeldKeys::default();
        held.observe_flags(ctrl().left_mask);
        assert!(held.is_held(KeyCode::ControlLeft));
        assert!(!held.is_held(KeyCode::ControlRight));
    }

    #[test]
    fn a_flags_change_adds_a_modifier_the_mask_says_is_down() {
        let mut held = HeldKeys::default();
        held.flags_changed(ctrl().left_mask, KeyCode::ControlLeft);
        assert!(held.pressed().contains(&KeyCode::ControlLeft));
        assert!(held.is_held(KeyCode::ControlLeft));
    }

    #[test]
    fn a_flags_change_removes_a_modifier_the_mask_says_is_up() {
        let mut held = HeldKeys::default();
        held.flags_changed(ctrl().left_mask, KeyCode::ControlLeft);
        held.flags_changed(0, KeyCode::ControlLeft);
        assert!(!held.pressed().contains(&KeyCode::ControlLeft));
        assert!(!held.is_held(KeyCode::ControlLeft));
    }

    /// A flags-changed event naming a non-modifier would mean macOS had reclassified the key. Acting
    /// on it drops a real key press, so it is ignored — but the mask still updates, because the mask
    /// is right about the modifiers whatever it says about the key.
    #[test]
    fn a_flags_change_for_an_ordinary_key_leaves_the_key_set_alone() {
        let mut held = HeldKeys::default();
        held.key_down(KeyCode::KeyA);

        held.flags_changed(ctrl().left_mask, KeyCode::KeyA);

        assert!(held.is_held(KeyCode::KeyA), "the real key press survives");
        assert_eq!(held.flags(), ctrl().left_mask, "and the mask is taken anyway");
    }

    /// The failure reconciliation exists for: a dropped event leaves a modifier stuck down, and a
    /// stuck modifier means every later binding is evaluated as though it were held.
    #[test]
    fn reconciling_drops_a_modifier_the_mask_no_longer_holds() {
        let mut held = HeldKeys::default();
        held.flags_changed(ctrl().left_mask, KeyCode::ControlLeft);
        assert!(held.pressed().contains(&KeyCode::ControlLeft));

        held.observe_flags(0);
        held.reconcile_modifiers();

        assert!(held.pressed().is_empty(), "the stuck modifier is gone");
    }

    /// Reconciling must NOT drop ordinary keys: the flags say nothing about them, so removing them
    /// would forget a key that really is down.
    #[test]
    fn reconciling_leaves_ordinary_keys_alone() {
        let mut held = HeldKeys::default();
        held.key_down(KeyCode::KeyA);

        held.observe_flags(0);
        held.reconcile_modifiers();

        assert!(held.is_held(KeyCode::KeyA));
    }

    /// A tap that was off missed any number of key-ups, so the whole cache is suspect and is thrown
    /// away rather than reconciled. The flags are a level and survive.
    #[test]
    fn a_re_enabled_tap_throws_the_whole_cache_away_and_trusts_the_mask() {
        let mut held = HeldKeys::default();
        held.key_down(KeyCode::KeyA);
        held.key_down(KeyCode::ControlLeft);

        held.tap_re_enabled(ctrl().left_mask);

        assert!(held.pressed().is_empty(), "every cached key is discarded");
        assert!(
            !held.is_held(KeyCode::KeyA),
            "including one that may still be down"
        );
        assert!(held.is_held(KeyCode::ControlLeft), "but the mask is believed");
    }

    /// The lock keys are reported as flags and never as edges, so a binding naming one is satisfied by
    /// the mask alone. A cache entry for one would never appear.
    #[test]
    fn a_lock_key_is_held_by_its_flag_alone() {
        let mut held = HeldKeys::default();
        assert!(!held.is_held(KeyCode::CapsLock));
        held.observe_flags(0x0001_0000);
        assert!(held.is_held(KeyCode::CapsLock), "MaskAlphaShift");
    }

    #[test]
    fn observing_flags_never_touches_the_key_set() {
        let mut held = HeldKeys::default();
        held.key_down(KeyCode::KeyA);
        held.observe_flags(ctrl().left_mask);
        assert!(held.pressed().contains(&KeyCode::KeyA));
        assert_eq!(held.flags(), ctrl().left_mask);
    }
}
