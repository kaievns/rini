//! The switcher's hold-to-reveal session, as a decision the tap can make immediately.
//!
//! The behaviour asked for is macOS's: a quick tap steps to the previous window, and holding the
//! modifier keeps a switch open so further presses move a selection that only commits when the
//! modifier is released. So the tap has to know, for every key event, whether a switch is live and
//! whether this key belongs to it — and answer without doing any work, because an active event tap sits
//! in the delivery path and the window server blocks each matching event until the callback returns.
//!
//! Which is why this holds nothing but a flag, a generation and a clock reading. The list of windows
//! and the cursor over it live where the popup will: on the other side of the channel. This side
//! decides only "swallow or pass" and "what to tell the reactor", which is a pure function of the key,
//! the modifier state and the elapsed time.
//!
//! Three rules earned the hard way, all of them about not stranding the user:
//!
//! 1. **The modifier's release is never swallowed.** Suppressing that `FlagsChanged` would leave every
//!    application believing the modifier is still held, forever. The commit rides ON the release and the
//!    release still goes through.
//! 2. **A session has a hard deadline**, checked against arriving events rather than kept by a timer.
//!    The tap can be rebuilt on a config reload, stood down for ten seconds by its own re-enable
//!    governor, or have its held-key cache wiped — and a session that believed the modifier was still
//!    down across any of those would swallow keys with nothing left to release it.
//! 3. **A key that means nothing to the switch passes through.** Swallowing broadly would be the more
//!    "correct" modal behaviour and is also how a live session turns into a dead keyboard.

use std::time::{Duration, Instant};

use crate::input::domain::hotkey::modifiers_satisfy;
use crate::input::domain::key::{KeyCode, Modifiers};

/// How long a switch may stay open with no further input before it commits itself.
///
/// The backstop for every way the tap can lose track of the modifier: a rebuild on config reload, the
/// re-enable governor's ten-second stand-down, a wiped held-key cache. Longer than any deliberate hold
/// and short enough that a session nobody is holding cannot sit on the arrow keys.
pub const MAX_SESSION: Duration = Duration::from_secs(12);

/// The keys a switch answers to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SwitchKeys {
    /// The chord that opens a switch and steps it forward.
    pub forward: KeyCode,
    /// The modifiers that must be held for `forward` to mean "switch".
    ///
    /// These are also what must STAY held: releasing them is the commit. A trigger with no modifiers
    /// cannot hold a session open at all, and [`SwitchKeys::can_hold`] says so.
    pub hold: Modifiers,
    /// Held alongside `hold`, steps backward instead. Shift, normally.
    pub backward: Modifiers,
}

impl SwitchKeys {
    /// Whether these keys can hold a session open.
    ///
    /// A trigger bound with no modifiers — a bare function key, say — has nothing whose release could
    /// commit, so it never opens a session and the one-shot binding handles it instead.
    pub fn can_hold(&self) -> bool {
        self.hold != Modifiers::empty()
    }
}

/// What the tap should tell the reactor.
///
/// Serialisable because the reactor's event enum is, for the record/replay harness.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Signal {
    /// Begin a switch. The reactor builds the list and selects the second entry, so an Open followed
    /// straight by a Commit is the quick tap.
    Open { backward: bool },
    /// Move the selection. Negative steps backward.
    Step(isize),
    /// Focus the selection and end the switch.
    Commit,
    /// End the switch, changing nothing.
    Cancel,
}

/// One key event, as the state machine needs it.
#[derive(Debug, Clone, Copy)]
pub struct KeyEvent {
    pub kind: KeyEventKind,
    pub key: KeyCode,
    /// Which modifiers are held right now, from the event's own flags — the authoritative level rather
    /// than a cache of edges.
    pub modifiers: Modifiers,
    pub is_repeat: bool,
    pub at: Instant,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyEventKind {
    Down,
    Up,
    /// A modifier changed. Its release is what commits a switch.
    FlagsChanged,
}

/// What the tap does with the event, and what it tells the reactor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Verdict {
    /// Delete the event rather than deliver it.
    ///
    /// Never true for a `FlagsChanged`: see rule 1 in the module docs.
    pub swallow: bool,
    pub signal: Option<Signal>,
}

impl Verdict {
    const PASS: Self = Self { swallow: false, signal: None };

    fn pass_with(signal: Signal) -> Self {
        Self {
            swallow: false,
            signal: Some(signal),
        }
    }

    fn eat_with(signal: Signal) -> Self {
        Self {
            swallow: true,
            signal: Some(signal),
        }
    }

    const fn eat() -> Self {
        Self { swallow: true, signal: None }
    }
}

/// Whether a switch is open, and since when.
#[derive(Debug, Default)]
pub struct SwitchSession {
    keys: Option<SwitchKeys>,
    live: Option<Live>,
    /// Counts opens for the lifetime of the tap, not of a session. Derived from `live` it was always
    /// 1, because `live` is `None` at the moment a session opens.
    opens: u64,
}

#[derive(Debug, Clone, Copy)]
struct Live {
    opened: Instant,
    /// Bumped on every open, so a stale reveal or commit arriving late can be recognised and dropped
    /// by the far side rather than acted on.
    generation: u64,
}

impl SwitchSession {
    /// Install the keys a switch answers to, ending any session in progress.
    ///
    /// Called when the bindings change. Ending the session is the point: the keys it was watching for
    /// may no longer exist, so there would be nothing left to close it.
    pub fn set_keys(&mut self, keys: Option<SwitchKeys>) -> Option<Signal> {
        self.keys = keys;
        self.live.take().map(|_| Signal::Cancel)
    }

    pub fn is_live(&self) -> bool {
        self.live.is_some()
    }

    pub fn generation(&self) -> u64 {
        self.live.map(|live| live.generation).unwrap_or(0)
    }

    /// End a live session because the tap can no longer be trusted about what is held.
    ///
    /// Called when the tap is rebuilt or re-enabled. Commits rather than cancels: the user pressed the
    /// key meaning to go somewhere, and losing the keyboard is not a reason to pretend they did not.
    pub fn abandon(&mut self) -> Option<Signal> {
        self.live.take().map(|_| Signal::Commit)
    }

    /// The whole decision, for one key event.
    pub fn on_key(&mut self, event: KeyEvent) -> Verdict {
        let Some(keys) = self.keys.clone() else {
            return Verdict::PASS;
        };

        // The deadline first, so an event arriving after one is the thing that closes the session
        // rather than being swallowed by it.
        if let Some(live) = self.live {
            if event.at.saturating_duration_since(live.opened) >= MAX_SESSION {
                self.live = None;
                // Pass the event through: it is not part of the switch any more, and if it happens to
                // be the trigger the next branch would have opened a fresh session on a key the user
                // is still holding from the last one.
                return Verdict::pass_with(Signal::Commit);
            }
        }

        let hold_held = modifiers_satisfy(keys.hold, event.modifiers);

        match event.kind {
            KeyEventKind::FlagsChanged => {
                // The commit. Never swallowed, whatever else is true.
                if self.live.is_some() && !hold_held {
                    self.live = None;
                    return Verdict::pass_with(Signal::Commit);
                }
                Verdict::PASS
            }
            KeyEventKind::Up => {
                // The trigger's own release, whose press was swallowed. Swallow it too rather than
                // leaving the focused application an orphan key-up for a key it never saw pressed.
                if self.live.is_some() && event.key == keys.forward {
                    return Verdict::eat();
                }
                Verdict::PASS
            }
            KeyEventKind::Down => self.on_key_down(&keys, event, hold_held),
        }
    }

    fn on_key_down(&mut self, keys: &SwitchKeys, event: KeyEvent, hold_held: bool) -> Verdict {
        let backward = modifiers_satisfy(keys.backward, event.modifiers);

        if event.key == keys.forward && hold_held {
            return match self.live {
                // Already open: another press steps the selection. Repeats included, which is what
                // makes holding the key walk the list the way the native switcher does.
                Some(_) => Verdict::eat_with(Signal::Step(if backward { -1 } else { 1 })),
                None => {
                    if !keys.can_hold() {
                        // Nothing to release, so nothing could ever commit. Leave it to the one-shot
                        // binding rather than opening a session that cannot close.
                        return Verdict::PASS;
                    }
                    self.opens = self.opens.wrapping_add(1);
                    self.live = Some(Live {
                        opened: event.at,
                        generation: self.opens,
                    });
                    Verdict::eat_with(Signal::Open { backward })
                }
            };
        }

        // Everything below only means anything while a switch is open.
        if self.live.is_none() {
            return Verdict::PASS;
        }

        match event.key {
            KeyCode::ArrowLeft | KeyCode::ArrowUp => Verdict::eat_with(Signal::Step(-1)),
            KeyCode::ArrowRight | KeyCode::ArrowDown => Verdict::eat_with(Signal::Step(1)),
            KeyCode::Escape => {
                self.live = None;
                Verdict::eat_with(Signal::Cancel)
            }
            // A key the switch has no use for. Passed through rather than eaten: swallowing
            // everything would be the more thorough modal behaviour and is also how a session that
            // outlives its modifier becomes a dead keyboard.
            _ => Verdict::PASS,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys() -> SwitchKeys {
        SwitchKeys {
            forward: KeyCode::KeyQ,
            hold: Modifiers::CONTROL,
            backward: Modifiers::SHIFT,
        }
    }

    fn session() -> SwitchSession {
        let mut session = SwitchSession::default();
        assert_eq!(session.set_keys(Some(keys())), None);
        session
    }

    fn at(base: Instant, millis: u64) -> Instant {
        base + Duration::from_millis(millis)
    }

    fn down(key: KeyCode, modifiers: Modifiers, when: Instant) -> KeyEvent {
        KeyEvent {
            kind: KeyEventKind::Down,
            key,
            modifiers,
            is_repeat: false,
            at: when,
        }
    }

    fn flags(modifiers: Modifiers, when: Instant) -> KeyEvent {
        KeyEvent {
            kind: KeyEventKind::FlagsChanged,
            key: KeyCode::ControlLeft,
            modifiers,
            is_repeat: false,
            at: when,
        }
    }

    /// The quick tap: press the chord, release the modifier, land on the previous window. Open selects
    /// the second entry on the far side, so Open-then-Commit is the whole gesture.
    #[test]
    fn a_tap_opens_and_commits() {
        let base = Instant::now();
        let mut session = session();

        let opened = session.on_key(down(KeyCode::KeyQ, Modifiers::CONTROL_LEFT, base));
        assert_eq!(
            opened,
            Verdict {
                swallow: true,
                signal: Some(Signal::Open { backward: false })
            }
        );
        assert!(session.is_live());

        let committed = session.on_key(flags(Modifiers::empty(), at(base, 80)));
        assert_eq!(
            committed,
            Verdict {
                swallow: false,
                signal: Some(Signal::Commit)
            }
        );
        assert!(!session.is_live());
    }

    /// Rule 1. Swallowing the modifier's release would leave every application believing it is still
    /// held, so the commit rides on an event that still gets delivered.
    #[test]
    fn the_modifier_release_is_never_swallowed() {
        let base = Instant::now();
        let mut session = session();
        session.on_key(down(KeyCode::KeyQ, Modifiers::CONTROL_LEFT, base));

        let verdict = session.on_key(flags(Modifiers::empty(), at(base, 50)));

        assert!(!verdict.swallow, "the release must reach the applications");
    }

    #[test]
    fn a_further_press_while_held_steps_the_selection() {
        let base = Instant::now();
        let mut session = session();
        session.on_key(down(KeyCode::KeyQ, Modifiers::CONTROL_LEFT, base));

        let stepped = session.on_key(down(KeyCode::KeyQ, Modifiers::CONTROL_LEFT, at(base, 60)));

        assert_eq!(
            stepped,
            Verdict {
                swallow: true,
                signal: Some(Signal::Step(1))
            }
        );
        assert!(session.is_live(), "and the switch stays open");
    }

    /// Holding the key repeats it, and the native switcher advances on every repeat.
    #[test]
    fn a_repeat_steps_too() {
        let base = Instant::now();
        let mut session = session();
        session.on_key(down(KeyCode::KeyQ, Modifiers::CONTROL_LEFT, base));

        let mut repeat = down(KeyCode::KeyQ, Modifiers::CONTROL_LEFT, at(base, 90));
        repeat.is_repeat = true;

        assert_eq!(
            session.on_key(repeat),
            Verdict {
                swallow: true,
                signal: Some(Signal::Step(1))
            }
        );
    }

    #[test]
    fn shift_steps_backward() {
        let base = Instant::now();
        let mut session = session();
        let mut held = Modifiers::CONTROL_LEFT;
        held.insert(Modifiers::SHIFT_LEFT);

        let opened = session.on_key(down(KeyCode::KeyQ, held, base));
        assert_eq!(
            opened,
            Verdict {
                swallow: true,
                signal: Some(Signal::Open { backward: true })
            }
        );

        assert_eq!(
            session.on_key(down(KeyCode::KeyQ, held, at(base, 60))),
            Verdict {
                swallow: true,
                signal: Some(Signal::Step(-1))
            }
        );
    }

    #[test]
    fn the_arrow_keys_move_the_selection_while_a_switch_is_open() {
        let base = Instant::now();
        let mut session = session();
        session.on_key(down(KeyCode::KeyQ, Modifiers::CONTROL_LEFT, base));

        for (key, expected) in [
            (KeyCode::ArrowRight, 1isize),
            (KeyCode::ArrowDown, 1),
            (KeyCode::ArrowLeft, -1),
            (KeyCode::ArrowUp, -1),
        ] {
            assert_eq!(
                session.on_key(down(key, Modifiers::CONTROL_LEFT, at(base, 100))),
                Verdict {
                    swallow: true,
                    signal: Some(Signal::Step(expected))
                },
                "{key:?}"
            );
        }
    }

    /// With no switch open the arrows belong to whatever is focused.
    #[test]
    fn the_arrow_keys_are_untouched_when_no_switch_is_open() {
        let base = Instant::now();
        let mut session = session();

        assert_eq!(
            session.on_key(down(KeyCode::ArrowRight, Modifiers::empty(), base)),
            Verdict::PASS
        );
    }

    #[test]
    fn escape_cancels_without_committing() {
        let base = Instant::now();
        let mut session = session();
        session.on_key(down(KeyCode::KeyQ, Modifiers::CONTROL_LEFT, base));

        let verdict = session.on_key(down(KeyCode::Escape, Modifiers::CONTROL_LEFT, at(base, 40)));

        assert_eq!(
            verdict,
            Verdict {
                swallow: true,
                signal: Some(Signal::Cancel)
            }
        );
        assert!(!session.is_live());
    }

    /// Rule 3. Swallowing every key while a switch is open would be the more thorough modal behaviour
    /// and is also how a session that outlives its modifier becomes a dead keyboard.
    #[test]
    fn a_key_the_switch_has_no_use_for_passes_through() {
        let base = Instant::now();
        let mut session = session();
        session.on_key(down(KeyCode::KeyQ, Modifiers::CONTROL_LEFT, base));

        let verdict = session.on_key(down(KeyCode::KeyA, Modifiers::CONTROL_LEFT, at(base, 30)));

        assert_eq!(verdict, Verdict::PASS);
        assert!(session.is_live(), "and it does not end the switch either");
    }

    /// Rule 2. The tap can be rebuilt, stood down for ten seconds, or have its held-key cache wiped,
    /// and a session that believed the modifier was still held would swallow the arrows with nothing
    /// left to release it. The deadline is checked against arriving events, so it needs no timer.
    #[test]
    fn a_session_past_its_deadline_commits_on_the_next_event() {
        let base = Instant::now();
        let mut session = session();
        session.on_key(down(KeyCode::KeyQ, Modifiers::CONTROL_LEFT, base));

        let late = down(KeyCode::ArrowRight, Modifiers::CONTROL_LEFT, base + MAX_SESSION);
        let verdict = session.on_key(late);

        assert_eq!(
            verdict,
            Verdict {
                swallow: false,
                signal: Some(Signal::Commit)
            },
            "the late event closes the session and is passed through, not eaten by it"
        );
        assert!(!session.is_live());
    }

    /// And the event that closes an expired session must not immediately open a new one — the user is
    /// still holding the key from the session that just expired.
    #[test]
    fn the_trigger_arriving_late_closes_rather_than_reopens() {
        let base = Instant::now();
        let mut session = session();
        session.on_key(down(KeyCode::KeyQ, Modifiers::CONTROL_LEFT, base));

        let late = down(KeyCode::KeyQ, Modifiers::CONTROL_LEFT, base + MAX_SESSION);
        assert_eq!(
            session.on_key(late),
            Verdict {
                swallow: false,
                signal: Some(Signal::Commit)
            }
        );
        assert!(!session.is_live());
    }

    /// Losing the keyboard is not a reason to pretend the user did not ask to go somewhere.
    #[test]
    fn abandoning_a_session_commits_it() {
        let base = Instant::now();
        let mut session = session();
        session.on_key(down(KeyCode::KeyQ, Modifiers::CONTROL_LEFT, base));

        assert_eq!(session.abandon(), Some(Signal::Commit));
        assert!(!session.is_live());
        assert_eq!(session.abandon(), None, "and it is idempotent");
    }

    /// The keys it was watching for may no longer exist, so there would be nothing left to close it.
    #[test]
    fn changing_the_keys_cancels_a_session_in_progress() {
        let base = Instant::now();
        let mut session = session();
        session.on_key(down(KeyCode::KeyQ, Modifiers::CONTROL_LEFT, base));

        assert_eq!(session.set_keys(Some(keys())), Some(Signal::Cancel));
        assert!(!session.is_live());
    }

    #[test]
    fn with_no_keys_configured_nothing_is_ever_touched() {
        let base = Instant::now();
        let mut session = SwitchSession::default();

        assert_eq!(
            session.on_key(down(KeyCode::KeyQ, Modifiers::CONTROL_LEFT, base)),
            Verdict::PASS
        );
        assert!(!session.is_live());
    }

    /// A trigger bound with no modifiers has nothing whose release could commit, so it must not open a
    /// session at all — the one-shot binding handles it instead.
    #[test]
    fn a_trigger_with_nothing_to_hold_never_opens_a_session() {
        let base = Instant::now();
        let mut session = SwitchSession::default();
        session.set_keys(Some(SwitchKeys {
            forward: KeyCode::KeyQ,
            hold: Modifiers::empty(),
            backward: Modifiers::SHIFT,
        }));

        assert_eq!(
            session.on_key(down(KeyCode::KeyQ, Modifiers::empty(), base)),
            Verdict::PASS
        );
        assert!(!session.is_live());
    }

    /// The trigger without its modifier is not the trigger. It belongs to whatever is focused.
    #[test]
    fn the_trigger_without_its_modifier_passes_through() {
        let base = Instant::now();
        let mut session = session();

        assert_eq!(
            session.on_key(down(KeyCode::KeyQ, Modifiers::empty(), base)),
            Verdict::PASS
        );
        assert!(!session.is_live());
    }

    /// The press was swallowed, so its release must be too — otherwise the focused application gets a
    /// key-up for a key it never saw pressed.
    #[test]
    fn the_triggers_own_release_is_swallowed_while_a_switch_is_open() {
        let base = Instant::now();
        let mut session = session();
        session.on_key(down(KeyCode::KeyQ, Modifiers::CONTROL_LEFT, base));

        let verdict = session.on_key(KeyEvent {
            kind: KeyEventKind::Up,
            key: KeyCode::KeyQ,
            modifiers: Modifiers::CONTROL_LEFT,
            is_repeat: false,
            at: at(base, 20),
        });

        assert_eq!(verdict, Verdict::eat());
        assert!(
            session.is_live(),
            "releasing the trigger does not end the switch"
        );
    }

    /// A modifier changing while the hold is still down — adding shift for a backward step — is not a
    /// commit.
    #[test]
    fn a_modifier_change_that_keeps_the_hold_does_not_commit() {
        let base = Instant::now();
        let mut session = session();
        session.on_key(down(KeyCode::KeyQ, Modifiers::CONTROL_LEFT, base));

        let mut still_held = Modifiers::CONTROL_LEFT;
        still_held.insert(Modifiers::SHIFT_LEFT);
        let verdict = session.on_key(flags(still_held, at(base, 30)));

        assert_eq!(verdict, Verdict::PASS);
        assert!(session.is_live());
    }

    /// Either side of the modifier family holds the switch open, since a side-agnostic binding is
    /// satisfied by either.
    #[test]
    fn either_side_of_the_modifier_holds_the_switch() {
        let base = Instant::now();
        let mut session = session();

        assert!(
            session
                .on_key(down(KeyCode::KeyQ, Modifiers::CONTROL_RIGHT, base))
                .signal
                .is_some()
        );
        assert!(session.is_live());
    }

    #[test]
    fn a_commit_with_no_session_open_signals_nothing() {
        let base = Instant::now();
        let mut session = session();

        assert_eq!(session.on_key(flags(Modifiers::empty(), base)), Verdict::PASS);
    }

    /// Each open gets its own generation, so a reveal or commit that arrives late can be told from one
    /// belonging to the switch that is open now.
    #[test]
    fn every_open_gets_a_fresh_generation() {
        let base = Instant::now();
        let mut session = session();

        session.on_key(down(KeyCode::KeyQ, Modifiers::CONTROL_LEFT, base));
        let first = session.generation();
        session.on_key(flags(Modifiers::empty(), at(base, 20)));

        session.on_key(down(KeyCode::KeyQ, Modifiers::CONTROL_LEFT, at(base, 40)));
        assert_ne!(session.generation(), first);
    }
}
