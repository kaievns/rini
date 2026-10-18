//! What the tap does with each key event, in the order the rules are asked.
//!
//! The tap reads a CGEvent into a [`Keystroke`] and does what the [`Verdict`] says. Everything between
//! is here: which keys are held, the switcher's and the overview's session, the hotkey table, and
//! which of them answers first.
//!
//! 1. **rini's own events pass untouched.** A shortcut rini posts for an application has to reach
//!    it, not come back as a rini hotkey, and it says nothing about what the user holds.
//! 2. **The held keys are brought up to date before anything reads them**, from the event's own
//!    flags: a `FlagsChanged` describes the state after it, not before.
//! 3. **The session before the hotkey table.** The table swallows a held key's repeats unless its
//!    bind asks for them, because a repeat re-fired commands such as workspace back-and-forth. A
//!    switch needs repeats to walk the selection and gets them by being asked first, which leaves
//!    that rule as it is. The overview's session passes every key but its own, so a bind fires as it
//!    does outside it.
//! 4. **Only a key-down reaches the table.** A bound key's down is swallowed whether it fires or is a
//!    repeat; its up goes through.
//! 5. **A mouse event's flags drop a modifier the edges left stuck**, because macOS drops a key-up or
//!    a flags change after system UI, only when they differ from the last seen, as mouse events are
//!    the most frequent kind. They close an overview whose hold they show up whether or not they
//!    differ: a key-down records its flags first, so the click after it would otherwise match.

use std::time::Instant;

use crate::input::domain::binding::{Action, Bind};
use crate::input::domain::held_keys::HeldKeys;
use crate::input::domain::hotkey::{HotkeyTable, Press};
use crate::input::domain::key::{Hotkey, KeyCode};
use crate::input::domain::switch_session::{
    KeyEvent, KeyEventKind, Signal, SwitchKeys, SwitchSession,
};

/// One key event, as the tap read it.
#[derive(Debug, Clone, Copy)]
pub struct Keystroke {
    pub kind: KeyEventKind,
    /// `None` for a keycode rini has no name for.
    pub key: Option<KeyCode>,
    /// The event's modifier flags, as bits.
    pub flags: u64,
    /// The key is held and this is its autorepeat.
    pub repeat: bool,
    /// rini posted it, for an application.
    pub synthetic: bool,
    pub at: Instant,
}

/// What the tap does with a key event, and what it tells the application.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Verdict {
    /// Delete the event rather than deliver it.
    pub swallow: bool,
    /// For the switcher or the overview, sent first.
    pub signal: Option<Signal>,
    /// The bind the key fired.
    pub action: Option<Action>,
}

/// The tap's whole state about the keyboard.
#[derive(Debug, Default)]
pub struct KeyRouter {
    held: HeldKeys,
    switch: SwitchSession,
    binds: Vec<Bind>,
    hotkeys: HotkeyTable,
}

impl KeyRouter {
    /// The whole decision, for one key event.
    pub fn on_key(&mut self, stroke: Keystroke) -> Verdict {
        if stroke.synthetic {
            return Verdict::default();
        }
        self.held.observe_flags(stroke.flags);
        let Some(key) = stroke.key else {
            return Verdict::default();
        };
        match stroke.kind {
            KeyEventKind::Down => self.held.key_down(key),
            KeyEventKind::Up => self.held.key_up(key),
            KeyEventKind::FlagsChanged => self.held.flags_changed(stroke.flags, key),
        }
        let modifiers = self.held.modifiers();
        let switched = self.switch.on_key(KeyEvent {
            kind: stroke.kind,
            key,
            modifiers,
            at: stroke.at,
        });
        let mut verdict = Verdict {
            swallow: switched.swallow,
            signal: switched.signal,
            action: None,
        };
        if switched.swallow || stroke.kind != KeyEventKind::Down {
            return verdict;
        }
        match self.hotkeys.press(&Hotkey::new(modifiers, key), stroke.repeat) {
            Press::Unbound => {}
            Press::Swallowed => verdict.swallow = true,
            Press::Fired(action) => {
                verdict.swallow = true;
                verdict.action = Some(action);
            }
        }
        verdict
    }

    /// The flags a mouse event carries; the overview's close when they show its hold up.
    pub fn on_mouse_flags(&mut self, flags: u64) -> Option<Signal> {
        if flags != self.held.flags() {
            self.held.observe_flags(flags);
            self.held.reconcile_modifiers();
        }
        self.switch.on_mouse(self.held.modifiers())
    }

    /// The tap came back after being off: any key-up may have been missed, so the held keys start
    /// again from `flags`, the live mask, and a live switch is abandoned.
    pub fn tap_re_enabled(&mut self, flags: u64) -> Option<Signal> {
        self.held.tap_re_enabled(flags);
        self.tap_replaced()
    }

    /// A new tap took the old one's place. The held keys survive it, a live session does not: one
    /// that crossed it believing its modifier was down would keep swallowing keys with nothing left
    /// to release it. A switch is committed rather than cancelled, because the user pressed the key
    /// meaning to go somewhere; the overview is closed.
    pub fn tap_replaced(&mut self) -> Option<Signal> { self.switch.abandon() }

    /// The keys each switcher and the overview answer to; a session in progress is ended.
    pub fn set_switch_keys(&mut self, keys: Vec<SwitchKeys>) -> Option<Signal> {
        self.switch.set_keys(keys)
    }

    /// The far side ended the session: a click on the popup committed the switch. True when one was
    /// open.
    pub fn close_switch(&mut self) -> bool { self.switch.close() }

    /// Every bind, its key free of `Mod`, resolved with `resolve`. Returns the keys it could not.
    pub fn set_binds(
        &mut self,
        binds: Vec<Bind>,
        resolve: impl Fn(&str) -> Option<Hotkey>,
    ) -> Vec<String> {
        self.binds = binds;
        self.resolve_binds(resolve)
    }

    /// The binds again, for a keyboard layout that names different physical keys.
    pub fn resolve_binds(&mut self, resolve: impl Fn(&str) -> Option<Hotkey>) -> Vec<String> {
        let (table, unresolved) = HotkeyTable::build(&self.binds, resolve);
        self.hotkeys = table;
        unresolved
    }

    /// How many chords the table answers to.
    pub fn hotkey_count(&self) -> usize { self.hotkeys.len() }

    #[cfg(test)]
    fn held(&self) -> &HeldKeys { &self.held }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use rini_ipc::protocol::SwitchScope;

    use super::*;
    use crate::input::domain::key::{MOD_FAMILIES, ModFamily, Modifiers};
    use crate::input::domain::switch_session::{MAX_SESSION, Opens, OverviewSignal};

    fn family(name: &str) -> &'static ModFamily {
        MOD_FAMILIES.iter().find(|f| f.name == name).expect("a family")
    }

    fn left(name: &str) -> u64 { family(name).mask | family(name).left_mask }

    fn resolve(spec: &str) -> Option<Hotkey> {
        match spec {
            "Ctrl+J" => Some(Hotkey::new(Modifiers::CONTROL, KeyCode::KeyJ)),
            "Ctrl+K" => Some(Hotkey::new(Modifiers::CONTROL, KeyCode::KeyK)),
            "Ctrl+Q" => Some(Hotkey::new(Modifiers::CONTROL, KeyCode::KeyQ)),
            "RAlt+J" => Some(Hotkey::new(Modifiers::ALT_RIGHT, KeyCode::KeyJ)),
            _ => None,
        }
    }

    fn bind(key: &str, action: Action, repeat: bool) -> Bind {
        Bind {
            key: key.into(),
            action,
            repeat,
        }
    }

    /// Ctrl+J and Ctrl+Q fire once, Ctrl+K on every repeat.
    fn router() -> KeyRouter {
        let mut router = KeyRouter::default();
        let unresolved = router.set_binds(
            vec![
                bind("Ctrl+J", Action::FocusColumnLeft, false),
                bind("Ctrl+K", Action::FocusColumnRight, true),
                bind("Ctrl+Q", Action::CloseWindow, false),
            ],
            resolve,
        );
        assert!(unresolved.is_empty());
        router
    }

    /// Ctrl+Q also opens the switcher, held with Ctrl and stepped back with Shift.
    fn switching(router: &mut KeyRouter) {
        let keys = SwitchKeys {
            opens: Opens::Switch {
                scope: SwitchScope::Everything,
                backward: Modifiers::SHIFT,
            },
            forward: KeyCode::KeyQ,
            hold: Modifiers::CONTROL,
        };
        assert_eq!(router.set_switch_keys(vec![keys]), None);
    }

    fn stroke(kind: KeyEventKind, key: KeyCode, flags: u64, at: Instant) -> Keystroke {
        Keystroke {
            kind,
            key: Some(key),
            flags,
            repeat: false,
            synthetic: false,
            at,
        }
    }

    fn down(key: KeyCode, flags: u64, at: Instant) -> Keystroke {
        stroke(KeyEventKind::Down, key, flags, at)
    }

    fn repeat(key: KeyCode, flags: u64, at: Instant) -> Keystroke {
        Keystroke {
            repeat: true,
            ..down(key, flags, at)
        }
    }

    fn fired(action: Action) -> Verdict {
        Verdict {
            swallow: true,
            signal: None,
            action: Some(action),
        }
    }

    fn signalled(swallow: bool, signal: Signal) -> Verdict {
        Verdict {
            swallow,
            signal: Some(signal),
            action: None,
        }
    }

    fn passed() -> Verdict { Verdict::default() }

    fn open() -> Signal {
        Signal::Open {
            backward: false,
            scope: SwitchScope::Everything,
        }
    }

    #[test]
    fn a_bound_key_fires_its_action_and_is_swallowed() {
        let now = Instant::now();
        let verdict = router().on_key(down(KeyCode::KeyJ, left("Ctrl"), now));
        assert_eq!(verdict, fired(Action::FocusColumnLeft));
    }

    #[test]
    fn an_unbound_key_goes_on_to_the_application() {
        let now = Instant::now();
        let mut router = router();
        assert_eq!(router.on_key(down(KeyCode::KeyL, left("Ctrl"), now)), passed());
        assert_eq!(router.on_key(down(KeyCode::KeyJ, 0, now)), passed());
    }

    #[test]
    fn a_synthetic_key_passes_untouched_though_bound() {
        let now = Instant::now();
        let mut router = router();
        let posted = Keystroke {
            synthetic: true,
            ..down(KeyCode::KeyJ, left("Ctrl"), now)
        };
        assert_eq!(router.on_key(posted), passed());
        assert!(router.held().pressed().is_empty(), "nor is it held");
        assert_eq!(router.held().flags(), 0, "nor are its flags taken");
    }

    #[test]
    fn a_synthetic_key_neither_opens_nor_steps_a_switch() {
        let now = Instant::now();
        let mut router = router();
        switching(&mut router);
        let posted = Keystroke {
            synthetic: true,
            ..down(KeyCode::KeyQ, left("Ctrl"), now)
        };
        assert_eq!(router.on_key(posted), passed());
        assert!(!router.close_switch(), "no session was opened");
    }

    #[test]
    fn a_repeat_of_a_bind_without_repeat_is_swallowed_and_fires_nothing() {
        let now = Instant::now();
        let mut router = router();
        assert_eq!(
            router.on_key(repeat(KeyCode::KeyJ, left("Ctrl"), now)),
            Verdict {
                swallow: true,
                signal: None,
                action: None,
            }
        );
    }

    #[test]
    fn a_bind_written_repeat_fires_on_every_repeat() {
        let now = Instant::now();
        let verdict = router().on_key(repeat(KeyCode::KeyK, left("Ctrl"), now));
        assert_eq!(verdict, fired(Action::FocusColumnRight));
    }

    #[test]
    fn only_a_key_down_reaches_the_table() {
        let now = Instant::now();
        let mut router = router();
        let up = stroke(KeyEventKind::Up, KeyCode::KeyJ, left("Ctrl"), now);
        assert_eq!(router.on_key(up), passed());
    }

    /// Ctrl+Q is both a bind and the switcher's trigger, and the session answers.
    #[test]
    fn the_switch_session_is_asked_before_the_hotkey_table() {
        let now = Instant::now();
        let mut router = router();
        switching(&mut router);
        let verdict = router.on_key(down(KeyCode::KeyQ, left("Ctrl"), now));
        assert_eq!(verdict, signalled(true, open()));
    }

    /// The table would swallow the repeat and fire nothing; the session asked first steps on it.
    #[test]
    fn a_repeat_of_the_trigger_steps_a_live_switch() {
        let now = Instant::now();
        let mut router = router();
        switching(&mut router);
        router.on_key(down(KeyCode::KeyQ, left("Ctrl"), now));
        let verdict = router.on_key(repeat(KeyCode::KeyQ, left("Ctrl"), now));
        assert_eq!(verdict, signalled(true, Signal::Step(1)));
    }

    #[test]
    fn a_key_the_switch_passes_still_reaches_the_table() {
        let now = Instant::now();
        let mut router = router();
        switching(&mut router);
        router.on_key(down(KeyCode::KeyQ, left("Ctrl"), now));
        let verdict = router.on_key(down(KeyCode::KeyJ, left("Ctrl"), now));
        assert_eq!(verdict, fired(Action::FocusColumnLeft));
    }

    /// The key that closes a session on its deadline is not part of the switch, so the table gets
    /// it, and the commit is sent before the action.
    #[test]
    fn a_key_after_the_deadline_commits_and_still_fires_its_bind() {
        let now = Instant::now();
        let mut router = router();
        switching(&mut router);
        router.on_key(down(KeyCode::KeyQ, left("Ctrl"), now));
        let late = now + MAX_SESSION + Duration::from_millis(1);
        let verdict = router.on_key(down(KeyCode::KeyJ, left("Ctrl"), late));
        assert_eq!(verdict, Verdict {
            swallow: true,
            signal: Some(Signal::Commit),
            action: Some(Action::FocusColumnLeft),
        });
    }

    #[test]
    fn releasing_the_held_modifier_commits_and_goes_through() {
        let now = Instant::now();
        let mut router = router();
        switching(&mut router);
        router.on_key(down(KeyCode::KeyQ, left("Ctrl"), now));
        let release = stroke(KeyEventKind::FlagsChanged, KeyCode::ControlLeft, 0, now);
        assert_eq!(router.on_key(release), signalled(false, Signal::Commit));
    }

    /// A flags change describes the state after it, so its own flags are what it is read with.
    #[test]
    fn a_flags_change_is_read_with_its_own_flags() {
        let now = Instant::now();
        let mut router = router();
        let press = stroke(
            KeyEventKind::FlagsChanged,
            KeyCode::ControlLeft,
            left("Ctrl"),
            now,
        );
        assert_eq!(router.on_key(press), passed());
        assert!(router.held().pressed().contains(&KeyCode::ControlLeft));
    }

    #[test]
    fn a_key_with_no_name_takes_its_flags_and_nothing_else() {
        let now = Instant::now();
        let mut router = router();
        let nameless = Keystroke {
            key: None,
            ..down(KeyCode::KeyJ, left("Ctrl"), now)
        };
        assert_eq!(router.on_key(nameless), passed());
        assert!(router.held().pressed().is_empty());
        assert_eq!(router.held().flags(), left("Ctrl"));
    }

    /// Right Alt's up was dropped. A mouse event with no Alt in its flags clears it, so a later J
    /// whose flags carry only Alt's family bit is read as left Alt and the right-Alt bind stays quiet.
    #[test]
    fn a_mouse_events_flags_drop_a_modifier_left_stuck() {
        let alt = family("Alt");
        let now = Instant::now();
        let mut router = KeyRouter::default();
        router.set_binds(vec![bind("RAlt+J", Action::CloseWindow, false)], resolve);
        let right = alt.mask | alt.right_mask;
        router.on_key(stroke(KeyEventKind::FlagsChanged, KeyCode::AltRight, right, now));

        router.on_mouse_flags(0);

        assert!(router.held().pressed().is_empty());
        assert_eq!(router.on_key(down(KeyCode::KeyJ, alt.mask, now)), passed());
    }

    #[test]
    fn a_mouse_event_with_the_flags_already_seen_reconciles_nothing() {
        let now = Instant::now();
        let mut router = router();
        router.on_key(down(KeyCode::ControlLeft, 0, now));
        router.on_mouse_flags(0);
        assert!(router.held().pressed().contains(&KeyCode::ControlLeft));
    }

    #[test]
    fn a_re_enabled_tap_forgets_the_held_keys_and_commits_a_live_switch() {
        let now = Instant::now();
        let mut router = router();
        switching(&mut router);
        router.on_key(down(KeyCode::KeyA, left("Ctrl"), now));
        router.on_key(down(KeyCode::KeyQ, left("Ctrl"), now));
        let live = left("Shift") | left("Super");

        assert_eq!(router.tap_re_enabled(live), Some(Signal::Commit));

        assert!(router.held().pressed().is_empty());
        assert_eq!(router.held().flags(), live);
    }

    #[test]
    fn a_replaced_tap_keeps_the_held_keys_and_commits_a_live_switch() {
        let now = Instant::now();
        let mut router = router();
        switching(&mut router);
        router.on_key(down(KeyCode::KeyQ, left("Ctrl"), now));

        assert_eq!(router.tap_replaced(), Some(Signal::Commit));

        assert!(router.held().pressed().contains(&KeyCode::KeyQ));
        assert_eq!(router.tap_replaced(), None, "nothing left to commit");
    }

    #[test]
    fn new_switch_keys_cancel_a_live_switch() {
        let now = Instant::now();
        let mut router = router();
        switching(&mut router);
        router.on_key(down(KeyCode::KeyQ, left("Ctrl"), now));
        assert_eq!(router.set_switch_keys(Vec::new()), Some(Signal::Cancel));
    }

    /// A click on the popup committed the switch, so the arrows are the application's again.
    #[test]
    fn a_switch_closed_from_outside_lets_the_arrows_through() {
        let now = Instant::now();
        let mut router = router();
        switching(&mut router);
        router.on_key(down(KeyCode::KeyQ, left("Ctrl"), now));
        assert!(router.close_switch());
        let arrow = router.on_key(down(KeyCode::ArrowLeft, left("Ctrl"), now));
        assert_eq!(arrow, passed());
    }

    #[test]
    fn a_layout_change_resolves_the_same_binds_again() {
        let now = Instant::now();
        let mut router = router();
        let unresolved = router.resolve_binds(|spec| match spec {
            "Ctrl+J" => Some(Hotkey::new(Modifiers::CONTROL, KeyCode::KeyH)),
            _ => None,
        });
        assert_eq!(unresolved, ["Ctrl+K", "Ctrl+Q"]);
        assert_eq!(router.hotkey_count(), 3, "Ctrl+H on either side or both");
        assert_eq!(
            router.on_key(down(KeyCode::KeyH, left("Ctrl"), now)),
            fired(Action::FocusColumnLeft)
        );
        assert_eq!(router.on_key(down(KeyCode::KeyJ, left("Ctrl"), now)), passed());
    }
    /// Kai's binds: the overview on Alt+Slash, Alt+J focusing left and Alt+Shift+J moving left, and
    /// the switcher on Alt+Q.
    fn overviewing() -> KeyRouter {
        let mut router = KeyRouter::default();
        let unresolved = router.set_binds(
            vec![
                bind("Alt+Slash", Action::ToggleOverview, false),
                bind("Alt+J", Action::FocusColumnLeft, false),
                bind("Alt+Shift+J", Action::MoveWindowLeft, false),
            ],
            |spec| match spec {
                "Alt+Slash" => Some(Hotkey::new(Modifiers::ALT, KeyCode::Slash)),
                "Alt+J" => Some(Hotkey::new(Modifiers::ALT, KeyCode::KeyJ)),
                "Alt+Shift+J" => {
                    let mut held = Modifiers::ALT;
                    held.insert(Modifiers::SHIFT);
                    Some(Hotkey::new(held, KeyCode::KeyJ))
                }
                _ => None,
            },
        );
        assert!(unresolved.is_empty());
        let keys = vec![
            SwitchKeys {
                opens: Opens::Overview,
                forward: KeyCode::Slash,
                hold: Modifiers::ALT,
            },
            SwitchKeys {
                opens: Opens::Switch {
                    scope: SwitchScope::Everything,
                    backward: Modifiers::SHIFT,
                },
                forward: KeyCode::KeyQ,
                hold: Modifiers::ALT,
            },
        ];
        assert_eq!(router.set_switch_keys(keys), None);
        router
    }

    fn overview(signal: OverviewSignal) -> Signal { Signal::Overview(signal) }

    fn alt() -> u64 { left("Alt") }

    fn opened(router: &mut KeyRouter, at: Instant) {
        router.on_key(stroke(KeyEventKind::FlagsChanged, KeyCode::AltLeft, alt(), at));
        let verdict = router.on_key(down(KeyCode::Slash, alt(), at));
        assert_eq!(verdict, signalled(true, overview(OverviewSignal::Open)));
    }

    fn let_go(at: Instant) -> Keystroke {
        stroke(KeyEventKind::FlagsChanged, KeyCode::AltLeft, 0, at)
    }

    #[test]
    fn the_overview_opens_on_its_chord_and_closes_on_the_release_passed_through() {
        let now = Instant::now();
        let mut router = overviewing();
        opened(&mut router, now);

        let held = stroke(
            KeyEventKind::FlagsChanged,
            KeyCode::ShiftLeft,
            alt() | left("Shift"),
            now,
        );
        assert_eq!(router.on_key(held), passed(), "Shift joining Alt keeps it open");

        let released = router.on_key(let_go(now));
        assert_eq!(released, signalled(false, overview(OverviewSignal::Close)));
        assert!(!router.close_switch(), "and it is over");
    }

    /// Kai's ask: the same keys work inside it, Shift moves included, and act on the real layout.
    #[test]
    fn a_bound_key_inside_the_overview_fires_its_action() {
        let now = Instant::now();
        let mut router = overviewing();
        opened(&mut router, now);

        assert_eq!(
            router.on_key(down(KeyCode::KeyJ, alt(), now)),
            fired(Action::FocusColumnLeft)
        );
        assert_eq!(
            router.on_key(down(KeyCode::KeyJ, alt() | left("Shift"), now)),
            fired(Action::MoveWindowLeft)
        );
        let up = stroke(KeyEventKind::Up, KeyCode::KeyJ, alt(), now);
        assert_eq!(router.on_key(up), passed(), "a bound key's up goes through");
        assert!(router.close_switch(), "and the overview stays open");
    }

    #[test]
    fn an_unbound_key_inside_the_overview_passes() {
        let now = Instant::now();
        let mut router = overviewing();
        opened(&mut router, now);
        for key in [KeyCode::KeyA, KeyCode::ArrowLeft] {
            assert_eq!(router.on_key(down(key, alt(), now)), passed(), "{key:?}");
        }
        assert!(router.close_switch(), "and it is still open");
    }

    #[test]
    fn escape_closes_the_overview_and_is_swallowed() {
        let now = Instant::now();
        let mut router = overviewing();
        opened(&mut router, now);

        let escaped = router.on_key(down(KeyCode::Escape, alt(), now));
        assert_eq!(escaped, signalled(true, overview(OverviewSignal::Close)));
        assert_eq!(
            router.on_key(let_go(now)),
            passed(),
            "the release closes nothing again"
        );
    }

    /// Passed, a held Slash would reach the table and its toggle; it changes nothing instead.
    #[test]
    fn the_trigger_repeated_inside_the_overview_is_swallowed_and_changes_nothing() {
        let now = Instant::now();
        let mut router = overviewing();
        opened(&mut router, now);
        let eaten = Verdict {
            swallow: true,
            signal: None,
            action: None,
        };

        assert_eq!(router.on_key(repeat(KeyCode::Slash, alt(), now)), eaten);
        assert_eq!(router.on_key(down(KeyCode::Slash, alt(), now)), eaten);
        let up = stroke(KeyEventKind::Up, KeyCode::Slash, alt(), now);
        assert_eq!(router.on_key(up), eaten, "its key-up too");
        assert_eq!(
            router.on_key(let_go(now)),
            signalled(false, overview(OverviewSignal::Close)),
            "still the one overview"
        );
    }

    #[test]
    fn the_overview_closes_twelve_seconds_after_its_last_key() {
        let now = Instant::now();
        let mut router = overviewing();
        opened(&mut router, now);
        let pressed = now + Duration::from_secs(10);
        assert_eq!(
            router.on_key(down(KeyCode::KeyJ, alt(), pressed)),
            fired(Action::FocusColumnLeft)
        );
        let again = pressed + MAX_SESSION - Duration::from_millis(1);
        assert_eq!(
            router.on_key(down(KeyCode::KeyJ, alt(), again)),
            fired(Action::FocusColumnLeft),
            "21.999s after the open, 11.999s after its last key: still open"
        );

        let late = again + MAX_SESSION;
        assert_eq!(router.on_key(down(KeyCode::KeyJ, alt(), late)), Verdict {
            swallow: true,
            signal: Some(overview(OverviewSignal::Close)),
            action: Some(Action::FocusColumnLeft),
        });
    }

    /// Alt's release went missing; the next mouse event's flags show it up.
    #[test]
    fn a_mouse_event_with_the_modifier_up_closes_the_overview() {
        let now = Instant::now();
        let mut router = overviewing();
        opened(&mut router, now);

        assert_eq!(router.on_mouse_flags(alt()), None, "flags unchanged");
        assert_eq!(router.on_mouse_flags(0), Some(overview(OverviewSignal::Close)));
        assert!(!router.close_switch());
    }

    /// A key with no name records its flags and asks nothing more, so the click after it carries
    /// the flags already seen and still closes the overview.
    #[test]
    fn a_mouse_event_with_the_flags_already_seen_still_closes_the_overview() {
        let now = Instant::now();
        let mut router = overviewing();
        opened(&mut router, now);
        let nameless = Keystroke {
            key: None,
            ..down(KeyCode::KeyA, 0, now)
        };
        assert_eq!(router.on_key(nameless), passed());

        assert_eq!(router.on_mouse_flags(0), Some(overview(OverviewSignal::Close)));
        assert!(!router.close_switch());
    }

    /// Alt's release went missing: the next key closes the overview and goes on to the application.
    #[test]
    fn a_key_with_the_modifier_up_closes_the_overview() {
        let now = Instant::now();
        let mut router = overviewing();
        opened(&mut router, now);
        assert_eq!(
            router.on_key(down(KeyCode::KeyA, 0, now)),
            signalled(false, overview(OverviewSignal::Close))
        );
        assert_eq!(router.on_mouse_flags(0), None, "nothing left to close");
        assert!(!router.close_switch());
    }

    #[test]
    fn replacing_the_tap_closes_the_overview() {
        let now = Instant::now();
        let mut router = overviewing();
        opened(&mut router, now);

        assert_eq!(router.tap_replaced(), Some(overview(OverviewSignal::Close)));
        assert_eq!(router.tap_replaced(), None, "nothing left to close");

        opened(&mut router, now);
        assert_eq!(
            router.tap_re_enabled(alt()),
            Some(overview(OverviewSignal::Close))
        );
    }

    /// One session at a time: a switcher's trigger neither opens a switch nor reaches the table.
    #[test]
    fn a_switchers_trigger_inside_the_overview_is_swallowed() {
        let now = Instant::now();
        let mut router = overviewing();
        opened(&mut router, now);

        let verdict = router.on_key(down(KeyCode::KeyQ, alt(), now));
        assert_eq!(verdict, Verdict {
            swallow: true,
            signal: None,
            action: None,
        });
        assert_eq!(
            router.on_key(let_go(now)),
            signalled(false, overview(OverviewSignal::Close))
        );
    }
}
