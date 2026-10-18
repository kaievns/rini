//! The switcher's hold-to-reveal session and the overview's hold, as a decision the tap can make
//! immediately.
//!
//! The behaviour asked for is macOS's: a quick tap steps to the previous window, and holding the
//! modifier keeps a switch open so further presses move a selection that only commits when the
//! modifier is released. So the tap has to know, for every key event, whether a switch is live and
//! whether this key belongs to it — and answer without doing any work, because an active event tap sits
//! in the delivery path and the window server blocks each matching event until the callback returns.
//!
//! Which is why this holds nothing but a flag and a clock reading. The list of windows
//! and the cursor over it live where the popup will: on the other side of the channel. This side
//! decides only "swallow or pass" and "what to tell the reactor", which is a pure function of the key,
//! the modifier state and the elapsed time.
//!
//! Three rules earned the hard way, all of them about not stranding the user:
//!
//! 1. **The modifier's release is never swallowed.** Suppressing that `FlagsChanged` would leave every
//!    application believing the modifier is still held, forever. The commit rides ON the release and the
//!    release still goes through.
//! 2. **A switch has a hard deadline**, checked against arriving events rather than kept by a timer.
//!    The tap can be rebuilt on a config reload, stood down for ten seconds by its own re-enable
//!    governor, or have its held-key cache wiped — and a switch that believed the modifier was still
//!    down across any of those would swallow the arrow keys with nothing left to release it.
//! 3. **A key that means nothing to the switch passes through.** Swallowing broadly would be the more
//!    "correct" modal behaviour and is also how a live session turns into a dead keyboard.
//!
//! The overview is the same hold with nothing to step: its trigger opens it and its modifiers keep it
//! open, and every other key goes on to the hotkey table, looked up as pressed with `Mod` in place
//! of the hold (`with_mod_for_hold`), so a bind runs on the real layout whichever modifier holds it.
//! It has no deadline: it stays open for as long as its hold is down, and any key or mouse event
//! whose flags show the hold up closes it, as the tap replaced or re-enabled does
//! (`specs/overview.md`).

use std::time::{Duration, Instant};

use rini_ipc::protocol::SwitchScope;

use crate::input::domain::binding::{Action, Bind};
use crate::input::domain::hotkey::modifiers_satisfy;
use crate::input::domain::key::{KeyCode, MOD_FAMILIES, ModKey, Modifiers};

/// How long a switch may stay open before it commits itself. The overview has none.
///
/// The backstop for every way the tap can lose track of the modifier: a rebuild on config reload, the
/// re-enable governor's ten-second stand-down, a wiped held-key cache. Longer than any deliberate hold
/// and short enough that a session nobody is holding cannot sit on the arrow keys.
pub const MAX_SESSION: Duration = Duration::from_secs(12);

/// What a session's keys open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Opens {
    /// A switcher over `scope`. `backward`, held alongside the hold, steps it backward instead:
    /// Shift, normally.
    Switch {
        scope: SwitchScope,
        backward: Modifiers,
    },
    /// The overview, open while the hold is down. Inside it a key is looked up with `mod_key` in
    /// place of the hold.
    Overview { mod_key: ModKey },
}

impl Opens {
    /// How a session these keys opened ends when its hold is let go or lost: a switch commits.
    fn released(self) -> Signal {
        match self {
            Opens::Switch { .. } => Signal::Commit,
            Opens::Overview { .. } => Signal::Overview(OverviewSignal::Close),
        }
    }

    /// How it ends when its keys are replaced under it: a switch cancels.
    fn dropped(self) -> Signal {
        match self {
            Opens::Switch { .. } => Signal::Cancel,
            Opens::Overview { .. } => Signal::Overview(OverviewSignal::Close),
        }
    }
}

/// The keys one session answers to. There is one of these per switcher that has a binding, and one
/// for the overview when it has one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SwitchKeys {
    /// What these keys open.
    pub opens: Opens,
    /// The chord that opens a session, and steps a switch forward.
    pub forward: KeyCode,
    /// The modifiers that must be held for `forward` to open it.
    ///
    /// These are also what must STAY held: releasing them ends it. A trigger with no modifiers
    /// cannot hold a session open at all, and [`SwitchKeys::can_hold`] says so.
    pub hold: Modifiers,
}

impl SwitchKeys {
    /// Whether these keys can hold a session open.
    ///
    /// A trigger bound with no modifiers — a bare function key, say — has nothing whose release could
    /// commit, so it never opens a session and the one-shot binding handles it instead.
    pub fn can_hold(&self) -> bool { self.hold != Modifiers::empty() }
}

/// The keys the overview answers to, one entry per `toggle-overview` bind, its `Mod` already
/// resolved to `mod_key`: each one's key opens it and its modifiers hold it, so the chord is written
/// once.
///
/// `parse` turns a spec such as `"Alt+Slash"` into its modifiers and key, as the switchers' do
/// (`switcher::domain::trigger`), since which physical key a name means depends on the layout.
pub fn overview_keys<F>(bindings: &[Bind], mod_key: ModKey, parse: F) -> Vec<SwitchKeys>
where F: Fn(&str) -> Option<(Modifiers, KeyCode)> {
    bindings
        .iter()
        .filter(|bind| bind.action == Action::ToggleOverview)
        .filter_map(|bind| parse(&bind.key))
        .map(|(hold, forward)| SwitchKeys {
            opens: Opens::Overview { mod_key },
            forward,
            hold,
        })
        .collect()
}

/// The modifiers a key pressed with `held` inside the overview is looked up with: `held` without
/// every modifier `hold` names, and with `Mod` in their place. So with the hold on Cmd, J alone is
/// `Mod+J` and Shift+J `Mod+Shift+J`; with the hold on `Mod` itself the chord is `held` as it is,
/// sides and all, so an `AltRight+J` bind still fires on the right Alt key alone.
pub fn with_mod_for_hold(held: Modifiers, hold: Modifiers, mod_key: ModKey) -> Modifiers {
    let mod_family = mod_key.modifiers();
    let mut chord = held;
    for family in MOD_FAMILIES {
        if family.generic != mod_family && hold.intersects(family.generic) {
            chord.remove(family.generic);
        }
    }
    if !chord.intersects(mod_family)
        && let Some(family) = MOD_FAMILIES.iter().find(|family| family.generic == mod_family)
    {
        chord.insert(family.left);
    }
    chord
}

/// Which session the tap opened, counted from 1 since the tap started: the far side ends a session
/// by its number, so an end that reaches the tap after the next session opened ends nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionId(pub u64);

/// What the tap should tell the reactor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Signal {
    /// Begin switch `session` over `scope`. The reactor builds the list and selects the second
    /// entry, so an Open followed straight by a Commit is the quick tap.
    Open {
        backward: bool,
        scope: SwitchScope,
        session: SessionId,
    },
    /// Move the selection. Negative steps backward.
    Step(isize),
    /// Focus the selection and end the switch.
    Commit,
    /// End the switch, changing nothing.
    Cancel,
    /// The overview opened or closed.
    Overview(OverviewSignal),
}

/// What the tap tells the reactor about the overview.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverviewSignal {
    /// It opened as `session`.
    Open(SessionId),
    /// Its hold was let go or lost, Escape was pressed, or the tap stopped trusting the keys. Nothing
    /// is undone: every press inside it already ran.
    Close,
}

/// One key event, as the state machine needs it.
#[derive(Debug, Clone, Copy)]
pub struct KeyEvent {
    pub kind: KeyEventKind,
    pub key: KeyCode,
    /// Which modifiers are held right now, from the event's own flags — the authoritative level rather
    /// than a cache of edges.
    pub modifiers: Modifiers,
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

    const fn eat() -> Self { Self { swallow: true, signal: None } }
}

/// Whether a switch or the overview is open, and since when.
#[derive(Debug, Default)]
pub struct SwitchSession {
    /// One entry per switcher with a binding, and the overview's. At most one session is live,
    /// whichever opened it.
    keys: Vec<SwitchKeys>,
    live: Option<Live>,
    /// How many sessions have opened: the last one's number.
    opened: u64,
}

#[derive(Debug, Clone, Copy)]
struct Live {
    /// When it opened: a switch's deadline counts from here.
    since: Instant,
    /// Which entry of `keys` opened this session: its key steps it and its modifiers hold it.
    trigger: usize,
    session: SessionId,
}

impl SwitchSession {
    /// Install the keys the switchers answer to, ending any session in progress.
    ///
    /// Called when the bindings change. Ending the session is the point: the keys it was watching for
    /// may be gone, so there would be nothing left to close it.
    pub fn set_keys(&mut self, keys: Vec<SwitchKeys>) -> Option<Signal> {
        let dropped = self.live.take().map(|live| self.opens(live).dropped());
        self.keys = keys;
        dropped
    }

    pub fn is_live(&self) -> bool { self.live.is_some() }

    /// Whether the session live is the overview's.
    pub fn overview_is_live(&self) -> bool {
        self.live.is_some_and(|live| matches!(self.opens(live), Opens::Overview { .. }))
    }

    /// The modifiers a key held with `held` is looked up with while the overview is open
    /// (`with_mod_for_hold`); `None` while it is not.
    pub fn overview_chord(&self, held: Modifiers) -> Option<Modifiers> {
        let keys = &self.keys[self.live?.trigger];
        match keys.opens {
            Opens::Overview { mod_key } => Some(with_mod_for_hold(held, keys.hold, mod_key)),
            Opens::Switch { .. } => None,
        }
    }

    /// End a live session because the tap cannot be trusted about what is held.
    ///
    /// Called when the tap is rebuilt or re-enabled. A switch commits rather than cancels: the user
    /// pressed the key meaning to go somewhere, and losing the keyboard is not a reason to pretend
    /// they did not. The overview closes.
    pub fn abandon(&mut self) -> Option<Signal> {
        self.live.take().map(|live| self.opens(live).released())
    }

    /// End `session`, which the far side has already ended, with nothing to tell it.
    ///
    /// A click on a popup row commits from the reactor, not from here, so the tap has to stop treating
    /// the switch as open: otherwise it would keep swallowing the arrow keys until the modifier is let
    /// go, and send a second commit on that release. A later session is left open: the end crossed
    /// its open on the way, and closing it would leave the far side holding it open with nothing
    /// left to close it. True when `session` was live.
    pub fn close(&mut self, session: SessionId) -> bool {
        if !self.live.is_some_and(|live| live.session == session) {
            return false;
        }
        self.live = None;
        true
    }

    /// The whole decision, for one key event.
    pub fn on_key(&mut self, event: KeyEvent) -> Verdict {
        if self.keys.is_empty() {
            return Verdict::PASS;
        }

        // The deadline first, so an event arriving after one is the thing that closes the switch
        // rather than being swallowed by it.
        if let Some(live) = self.live
            && matches!(self.opens(live), Opens::Switch { .. })
            && event.at.saturating_duration_since(live.since) >= MAX_SESSION
        {
            self.live = None;
            // Pass the event through: the switch is over, and if the event happens to
            // be the trigger the next branch would have opened a fresh session on a key the user
            // is still holding from the last one.
            return Verdict::pass_with(self.opens(live).released());
        }

        let Some(live) = self.live else {
            return match event.kind {
                KeyEventKind::Down => self.open(event),
                KeyEventKind::Up | KeyEventKind::FlagsChanged => Verdict::PASS,
            };
        };
        let keys = &self.keys[live.trigger];
        let hold_held = modifiers_satisfy(keys.hold, event.modifiers);
        let (opens, forward) = (keys.opens, keys.forward);
        if matches!(opens, Opens::Overview { .. }) && !hold_held {
            return self.let_go(event, forward);
        }

        match event.kind {
            // The commit, or the overview's close. Never swallowed, whatever else is true.
            KeyEventKind::FlagsChanged if !hold_held => {
                self.live = None;
                Verdict::pass_with(opens.released())
            }
            KeyEventKind::FlagsChanged => Verdict::PASS,
            // The trigger's own release, whose press was swallowed. Swallow it too rather than
            // leaving the focused application an orphan key-up for a key it never saw pressed.
            KeyEventKind::Up if event.key == forward => Verdict::eat(),
            KeyEventKind::Up => Verdict::PASS,
            KeyEventKind::Down => match opens {
                Opens::Switch { backward, .. } => self.step(live, event, hold_held, backward),
                Opens::Overview { .. } => self.browse(event),
            },
        }
    }

    /// An event of any kind whose flags leave out the overview's hold closes it: its release went
    /// missing, and with no deadline nothing else would. A key-down goes on to the hotkey table, and
    /// the trigger's key-up is eaten as its press was.
    fn let_go(&mut self, event: KeyEvent, forward: KeyCode) -> Verdict {
        self.live = None;
        let close = Signal::Overview(OverviewSignal::Close);
        if event.kind == KeyEventKind::Up && event.key == forward {
            return Verdict::eat_with(close);
        }
        Verdict::pass_with(close)
    }

    /// The modifiers a mouse event's flags say are held. An overview whose hold they leave out is
    /// closed: its release went missing, and it would otherwise stay open, holding every flight.
    pub fn on_mouse(&mut self, modifiers: Modifiers) -> Option<Signal> {
        let live = self.live?;
        let keys = &self.keys[live.trigger];
        if !matches!(keys.opens, Opens::Overview { .. }) || modifiers_satisfy(keys.hold, modifiers)
        {
            return None;
        }
        self.live = None;
        Some(Signal::Overview(OverviewSignal::Close))
    }

    fn opens(&self, live: Live) -> Opens { self.keys[live.trigger].opens }

    /// A key going down with no switch open: open one if it is a trigger, pass it through otherwise.
    fn open(&mut self, event: KeyEvent) -> Verdict {
        let Some(trigger) = self.trigger_for(event) else {
            return Verdict::PASS;
        };
        let keys = &self.keys[trigger];
        if !keys.can_hold() {
            // Nothing to release, so nothing could ever commit. Leave it to the one-shot binding
            // rather than opening a session that cannot close.
            return Verdict::PASS;
        }
        self.opened += 1;
        let session = SessionId(self.opened);
        let signal = match keys.opens {
            Opens::Switch { scope, backward } => Signal::Open {
                backward: modifiers_satisfy(backward, event.modifiers),
                scope,
                session,
            },
            Opens::Overview { .. } => Signal::Overview(OverviewSignal::Open(session)),
        };
        self.live = Some(Live {
            since: event.at,
            trigger,
            session,
        });
        Verdict::eat_with(signal)
    }

    /// A key going down with a switch open.
    fn step(
        &mut self,
        live: Live,
        event: KeyEvent,
        hold_held: bool,
        backward: Modifiers,
    ) -> Verdict {
        if event.key == self.keys[live.trigger].forward && hold_held {
            // Another press steps the selection. Repeats included, which is what makes holding the key
            // walk the list the way the native switcher does.
            let backward = modifiers_satisfy(backward, event.modifiers);
            return Verdict::eat_with(Signal::Step(if backward { -1 } else { 1 }));
        }
        // Another switcher's trigger, or the overview's, pressed while this one is open. Eaten with
        // nothing sent: passed through it would reach the hotkey table and run that switcher's
        // one-shot step in the middle of this switch, focusing a window nobody has committed to.
        if self.trigger_for(event).is_some() {
            return Verdict::eat();
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

    /// A key going down with the overview open.
    fn browse(&mut self, event: KeyEvent) -> Verdict {
        // Its own trigger again, autorepeat included, or a switcher's: eaten with nothing sent.
        // Passed, a held trigger would reach the table, and a switcher's would open over the overview.
        if self.trigger_for(event).is_some() {
            return Verdict::eat();
        }
        match event.key {
            KeyCode::Escape => {
                self.live = None;
                Verdict::eat_with(Signal::Overview(OverviewSignal::Close))
            }
            // Every other key goes on to the hotkey table, which swallows one with no bind there.
            _ => Verdict::PASS,
        }
    }

    /// Which session this key press is the trigger of.
    ///
    /// The MOST specific match wins. Modifiers are matched as "at least these", so with `Ctrl + Q` and
    /// `Ctrl + Alt + Q` both bound, `Ctrl + Alt + Q` satisfies both — and it means the second one.
    fn trigger_for(&self, event: KeyEvent) -> Option<usize> {
        self.keys
            .iter()
            .enumerate()
            .filter(|(_, keys)| event.key == keys.forward)
            .filter(|(_, keys)| modifiers_satisfy(keys.hold, event.modifiers))
            .max_by_key(|(index, keys)| (kinds(keys.hold), std::cmp::Reverse(*index)))
            .map(|(index, _)| index)
    }
}

/// How many kinds of modifier a set names, whichever side: `Ctrl + Alt` is two, and so is
/// `ControlLeft + Alt`. Bits would count a sideless `Ctrl` as two, since it is both sides at once.
fn kinds(modifiers: Modifiers) -> usize {
    [
        Modifiers::SHIFT,
        Modifiers::CONTROL,
        Modifiers::ALT,
        Modifiers::META,
    ]
    .into_iter()
    .filter(|kind| modifiers.intersects(*kind))
    .count()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn switcher(scope: SwitchScope, forward: KeyCode, hold: Modifiers) -> SwitchKeys {
        SwitchKeys {
            opens: Opens::Switch {
                scope,
                backward: Modifiers::SHIFT,
            },
            forward,
            hold,
        }
    }

    fn keys() -> SwitchKeys { switcher(SwitchScope::Everything, KeyCode::KeyQ, Modifiers::CONTROL) }

    fn session() -> SwitchSession {
        let mut session = SwitchSession::default();
        assert_eq!(session.set_keys(vec![keys()]), None);
        session
    }

    fn at(base: Instant, millis: u64) -> Instant { base + Duration::from_millis(millis) }

    fn down(key: KeyCode, modifiers: Modifiers, when: Instant) -> KeyEvent {
        KeyEvent {
            kind: KeyEventKind::Down,
            key,
            modifiers,
            at: when,
        }
    }

    fn flags(modifiers: Modifiers, when: Instant) -> KeyEvent {
        KeyEvent {
            kind: KeyEventKind::FlagsChanged,
            key: KeyCode::ControlLeft,
            modifiers,
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
        assert_eq!(opened, Verdict {
            swallow: true,
            signal: Some(Signal::Open {
                backward: false,
                scope: SwitchScope::Everything,
                session: SessionId(1),
            })
        });
        assert!(session.is_live());

        let committed = session.on_key(flags(Modifiers::empty(), at(base, 80)));
        assert_eq!(committed, Verdict {
            swallow: false,
            signal: Some(Signal::Commit)
        });
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

        assert_eq!(stepped, Verdict {
            swallow: true,
            signal: Some(Signal::Step(1))
        });
        assert!(session.is_live(), "and the switch stays open");
    }

    /// Holding the key repeats it, and the native switcher advances on every repeat.
    #[test]
    fn a_repeat_steps_too() {
        let base = Instant::now();
        let mut session = session();
        session.on_key(down(KeyCode::KeyQ, Modifiers::CONTROL_LEFT, base));

        for millis in [500, 533, 566] {
            assert_eq!(
                session.on_key(down(KeyCode::KeyQ, Modifiers::CONTROL_LEFT, at(base, millis))),
                Verdict {
                    swallow: true,
                    signal: Some(Signal::Step(1))
                }
            );
        }
    }

    #[test]
    fn shift_steps_backward() {
        let base = Instant::now();
        let mut session = session();
        let mut held = Modifiers::CONTROL_LEFT;
        held.insert(Modifiers::SHIFT_LEFT);

        let opened = session.on_key(down(KeyCode::KeyQ, held, base));
        assert_eq!(opened, Verdict {
            swallow: true,
            signal: Some(Signal::Open {
                backward: true,
                scope: SwitchScope::Everything,
                session: SessionId(1),
            })
        });

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

        assert_eq!(verdict, Verdict {
            swallow: true,
            signal: Some(Signal::Cancel)
        });
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
        assert_eq!(session.on_key(late), Verdict {
            swallow: false,
            signal: Some(Signal::Commit)
        });
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

    /// The keys it was watching for may be gone, so there would be nothing left to close it.
    #[test]
    fn changing_the_keys_cancels_a_session_in_progress() {
        let base = Instant::now();
        let mut session = session();
        session.on_key(down(KeyCode::KeyQ, Modifiers::CONTROL_LEFT, base));

        assert_eq!(session.set_keys(vec![keys()]), Some(Signal::Cancel));
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
        session.set_keys(vec![switcher(
            SwitchScope::Everything,
            KeyCode::KeyQ,
            Modifiers::empty(),
        )]);

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

    fn three_switchers() -> SwitchSession {
        let mut session = SwitchSession::default();
        session.set_keys(vec![
            keys(),
            switcher(SwitchScope::Workspace, KeyCode::KeyW, Modifiers::CONTROL),
            switcher(SwitchScope::App, KeyCode::KeyQ, {
                let mut hold = Modifiers::CONTROL;
                hold.insert(Modifiers::ALT);
                hold
            }),
        ]);
        session
    }

    fn control_alt() -> Modifiers {
        let mut held = Modifiers::CONTROL_LEFT;
        held.insert(Modifiers::ALT_LEFT);
        held
    }

    /// Each switcher's trigger opens a switch over its own scope.
    #[test]
    fn each_trigger_opens_its_own_scope() {
        let base = Instant::now();
        let mut session = three_switchers();

        let opened = session.on_key(down(KeyCode::KeyW, Modifiers::CONTROL_LEFT, base));
        assert_eq!(
            opened.signal,
            Some(Signal::Open {
                backward: false,
                scope: SwitchScope::Workspace,
                session: SessionId(1),
            })
        );
        session.on_key(flags(Modifiers::empty(), at(base, 50)));

        let opened = session.on_key(down(KeyCode::KeyQ, Modifiers::CONTROL_LEFT, at(base, 100)));
        assert_eq!(
            opened.signal,
            Some(Signal::Open {
                backward: false,
                scope: SwitchScope::Everything,
                session: SessionId(2),
            })
        );
    }

    /// `Ctrl + Alt + Q` satisfies `Ctrl + Q` too, because modifiers match as "at least these". It means
    /// the binding that names both.
    #[test]
    fn the_most_specific_trigger_wins() {
        let base = Instant::now();
        let mut session = three_switchers();

        let opened = session.on_key(down(KeyCode::KeyQ, control_alt(), base));
        assert_eq!(
            opened.signal,
            Some(Signal::Open {
                backward: false,
                scope: SwitchScope::App,
                session: SessionId(1),
            })
        );
    }

    /// The session steps on the key that opened it, and is held by the modifiers that opened it.
    #[test]
    fn a_session_answers_to_the_trigger_that_opened_it() {
        let base = Instant::now();
        let mut session = three_switchers();
        session.on_key(down(KeyCode::KeyW, Modifiers::CONTROL_LEFT, base));

        let stepped = session.on_key(down(KeyCode::KeyW, Modifiers::CONTROL_LEFT, at(base, 40)));
        assert_eq!(stepped, Verdict::eat_with(Signal::Step(1)));
        let released = session.on_key(flags(Modifiers::empty(), at(base, 80)));
        assert_eq!(released, Verdict::pass_with(Signal::Commit));
    }

    /// Another switcher's trigger pressed mid-switch is eaten and sends nothing. Passed through it
    /// would reach the hotkey table and run that switcher's one-shot step in the middle of this one.
    #[test]
    fn another_switchers_trigger_mid_switch_does_nothing() {
        let base = Instant::now();
        let mut session = three_switchers();
        session.on_key(down(KeyCode::KeyW, Modifiers::CONTROL_LEFT, base));

        let other = session.on_key(down(KeyCode::KeyQ, Modifiers::CONTROL_LEFT, at(base, 40)));
        assert_eq!(other, Verdict::eat());
        assert!(session.is_live(), "and the open switch carries on");
    }

    /// A switch held by `Ctrl + Alt` commits when either is let go, not only when both are.
    #[test]
    fn letting_go_of_part_of_the_hold_commits() {
        let base = Instant::now();
        let mut session = three_switchers();
        session.on_key(down(KeyCode::KeyQ, control_alt(), base));

        let released = session.on_key(flags(Modifiers::CONTROL_LEFT, at(base, 60)));
        assert_eq!(released, Verdict::pass_with(Signal::Commit));
    }

    /// A switch committed by a click is over: the arrow keys go back to the application, and letting go
    /// of the modifier sends nothing more.
    #[test]
    fn closing_a_session_ends_it_without_a_signal() {
        let base = Instant::now();
        let mut session = session();
        session.on_key(down(KeyCode::KeyQ, Modifiers::CONTROL_LEFT, base));

        assert!(session.close(SessionId(1)));
        assert!(!session.is_live());
        assert_eq!(
            session.on_key(down(KeyCode::ArrowRight, Modifiers::CONTROL_LEFT, at(base, 40))),
            Verdict::PASS,
            "the arrow keys are the application's again"
        );
        assert_eq!(
            session.on_key(flags(Modifiers::empty(), at(base, 80))),
            Verdict::PASS,
            "and the release commits nothing a second time"
        );
        assert!(!session.close(SessionId(1)), "nothing left to close");
    }
    fn alt(when: Instant) -> KeyEvent {
        KeyEvent {
            kind: KeyEventKind::FlagsChanged,
            key: KeyCode::AltLeft,
            modifiers: Modifiers::ALT_LEFT,
            at: when,
        }
    }

    /// The switcher on Ctrl+Q and the overview on Alt+Slash.
    fn with_overview() -> SwitchSession {
        let mut session = SwitchSession::default();
        session.set_keys(vec![keys(), SwitchKeys {
            opens: Opens::Overview { mod_key: ModKey::Alt },
            forward: KeyCode::Slash,
            hold: Modifiers::ALT,
        }]);
        session
    }

    fn overview(signal: OverviewSignal) -> Signal { Signal::Overview(signal) }

    fn opened_overview(base: Instant) -> SwitchSession {
        let mut session = with_overview();
        let opened = session.on_key(down(KeyCode::Slash, Modifiers::ALT_LEFT, base));
        assert_eq!(
            opened,
            Verdict::eat_with(overview(OverviewSignal::Open(SessionId(1))))
        );
        session
    }

    /// The far side ends the session it names and no other: an end that crossed the next open on
    /// its way leaves that one open, held as before, and its release still closes it.
    #[test]
    fn an_end_for_a_session_already_over_leaves_the_next_one_open() {
        let base = Instant::now();
        let mut session = opened_overview(base);
        session.on_key(flags(Modifiers::empty(), at(base, 40)));
        let reopened = session.on_key(down(KeyCode::Slash, Modifiers::ALT_LEFT, at(base, 80)));
        assert_eq!(
            reopened,
            Verdict::eat_with(overview(OverviewSignal::Open(SessionId(2))))
        );

        assert!(!session.close(SessionId(1)), "the first is over");
        assert!(session.is_live(), "and the second open");
        assert_eq!(
            session.on_key(flags(Modifiers::empty(), at(base, 120))),
            Verdict::pass_with(overview(OverviewSignal::Close))
        );
    }

    /// The overview has no selection to step on the tap's side: every other key passes on to the
    /// hotkey table.
    #[test]
    fn inside_the_overview_every_other_key_passes() {
        let base = Instant::now();
        let mut session = opened_overview(base);
        for key in [KeyCode::KeyJ, KeyCode::ArrowRight, KeyCode::KeyA] {
            assert_eq!(
                session.on_key(down(key, Modifiers::ALT_LEFT, at(base, 40))),
                Verdict::PASS,
                "{key:?}"
            );
        }
        assert!(session.is_live());
    }

    /// Kai looks for as long as he holds Alt: keys and silence past `MAX_SESSION` with the hold
    /// down leave it open.
    #[test]
    fn the_overview_stays_open_while_its_hold_is_down_however_long() {
        let base = Instant::now();
        let mut session = opened_overview(base);
        let looked = base + MAX_SESSION + Duration::from_secs(8);
        assert_eq!(
            session.on_key(down(KeyCode::KeyL, Modifiers::ALT_LEFT, looked)),
            Verdict::PASS
        );
        assert_eq!(session.on_key(alt(looked + MAX_SESSION)), Verdict::PASS);
        assert!(session.is_live(), "Alt held throughout");
    }

    /// Alt's release went missing and Kai typed on: the first key with Alt up closes it and goes
    /// on to the table, so the keys after it cannot keep it open.
    #[test]
    fn a_key_without_the_hold_closes_the_overview_and_goes_on() {
        let base = Instant::now();
        let mut session = opened_overview(base);
        assert_eq!(
            session.on_key(down(KeyCode::KeyJ, Modifiers::empty(), at(base, 40))),
            Verdict::pass_with(overview(OverviewSignal::Close))
        );
        assert!(!session.is_live());

        for seconds in [5, 10, 15, 20] {
            let typed = base + Duration::from_secs(seconds);
            assert_eq!(
                session.on_key(down(KeyCode::KeyH, Modifiers::empty(), typed)),
                Verdict::PASS
            );
            assert!(!session.is_live(), "{seconds}s in");
        }
    }

    /// The trigger's press was swallowed, so its key-up with the hold gone is too, as it closes.
    #[test]
    fn the_triggers_release_without_the_hold_closes_the_overview_and_is_swallowed() {
        let base = Instant::now();
        let mut session = opened_overview(base);
        let up = KeyEvent {
            kind: KeyEventKind::Up,
            key: KeyCode::Slash,
            modifiers: Modifiers::empty(),
            at: at(base, 30),
        };
        assert_eq!(
            session.on_key(up),
            Verdict::eat_with(overview(OverviewSignal::Close))
        );
        assert!(!session.is_live());
    }

    /// A switch keeps counting from its open, however many keys it hears.
    #[test]
    fn a_switchs_deadline_still_counts_from_its_open() {
        let base = Instant::now();
        let mut session = with_overview();
        session.on_key(down(KeyCode::KeyQ, Modifiers::CONTROL_LEFT, base));
        session.on_key(down(KeyCode::KeyA, Modifiers::CONTROL_LEFT, at(base, 11_000)));

        let late = down(KeyCode::KeyA, Modifiers::CONTROL_LEFT, base + MAX_SESSION);
        assert_eq!(session.on_key(late), Verdict::pass_with(Signal::Commit));
    }

    /// The trigger's press was swallowed, so its release is too.
    #[test]
    fn the_overviews_trigger_release_is_swallowed() {
        let base = Instant::now();
        let mut session = opened_overview(base);
        let up = KeyEvent {
            kind: KeyEventKind::Up,
            key: KeyCode::Slash,
            modifiers: Modifiers::ALT_LEFT,
            at: at(base, 30),
        };
        assert_eq!(session.on_key(up), Verdict::eat());
        assert!(session.is_live());
    }

    /// One session at a time: the overview's trigger pressed mid-switch is eaten with nothing sent.
    #[test]
    fn the_overviews_trigger_mid_switch_does_nothing() {
        let base = Instant::now();
        let mut session = SwitchSession::default();
        session.set_keys(vec![
            switcher(SwitchScope::Everything, KeyCode::KeyQ, Modifiers::ALT),
            SwitchKeys {
                opens: Opens::Overview { mod_key: ModKey::Alt },
                forward: KeyCode::Slash,
                hold: Modifiers::ALT,
            },
        ]);
        session.on_key(down(KeyCode::KeyQ, Modifiers::ALT_LEFT, base));

        let verdict = session.on_key(down(KeyCode::Slash, Modifiers::ALT_LEFT, at(base, 40)));

        assert_eq!(verdict, Verdict::eat());
        assert_eq!(
            session.on_key(KeyEvent {
                modifiers: Modifiers::empty(),
                ..alt(at(base, 60))
            }),
            Verdict::pass_with(Signal::Commit),
            "the switch is still the session"
        );
    }

    /// New keys close an overview in progress, as they cancel a switch.
    #[test]
    fn changing_the_keys_closes_the_overview() {
        let base = Instant::now();
        let mut session = opened_overview(base);
        assert_eq!(
            session.set_keys(Vec::new()),
            Some(overview(OverviewSignal::Close))
        );
        assert!(!session.is_live());
    }

    /// A mouse event's flags close only an overview whose hold they leave out.
    #[test]
    fn a_mouse_event_closes_only_an_overview_it_shows_let_go() {
        let base = Instant::now();
        let mut session = opened_overview(base);
        assert_eq!(session.on_mouse(Modifiers::ALT_LEFT), None, "Alt still held");
        assert!(session.is_live());
        assert_eq!(
            session.on_mouse(Modifiers::empty()),
            Some(overview(OverviewSignal::Close))
        );
        assert!(!session.is_live());
        assert_eq!(session.on_mouse(Modifiers::empty()), None, "nothing left");

        session.on_key(down(KeyCode::KeyQ, Modifiers::CONTROL_LEFT, base));
        assert_eq!(
            session.on_mouse(Modifiers::empty()),
            None,
            "a switch is not asked"
        );
        assert!(session.is_live());
    }
    fn both(first: Modifiers, second: Modifiers) -> Modifiers {
        let mut held = first;
        held.insert(second);
        held
    }

    /// The hold's modifiers give way to `Mod`, either side of the hold, and a `Mod` held besides
    /// keeps its own side; a hold on `Mod` itself leaves the chord as held, sides and all;
    /// everything else held stays.
    #[test]
    fn inside_the_overview_mod_stands_in_for_the_hold() {
        let rows = [
            (
                Modifiers::META_LEFT,
                Modifiers::META,
                ModKey::Alt,
                Modifiers::ALT_LEFT,
            ),
            (
                both(Modifiers::META_RIGHT, Modifiers::SHIFT_LEFT),
                Modifiers::META,
                ModKey::Alt,
                both(Modifiers::ALT_LEFT, Modifiers::SHIFT_LEFT),
            ),
            (
                both(Modifiers::META_LEFT, Modifiers::ALT_RIGHT),
                Modifiers::META,
                ModKey::Alt,
                Modifiers::ALT_RIGHT,
            ),
            (
                Modifiers::ALT_LEFT,
                Modifiers::ALT,
                ModKey::Alt,
                Modifiers::ALT_LEFT,
            ),
            (
                Modifiers::ALT_RIGHT,
                Modifiers::ALT,
                ModKey::Alt,
                Modifiers::ALT_RIGHT,
            ),
            (
                both(Modifiers::ALT_LEFT, Modifiers::ALT_RIGHT),
                Modifiers::ALT_LEFT,
                ModKey::Alt,
                both(Modifiers::ALT_LEFT, Modifiers::ALT_RIGHT),
            ),
            (
                both(Modifiers::ALT_LEFT, Modifiers::SHIFT_RIGHT),
                Modifiers::ALT,
                ModKey::Alt,
                both(Modifiers::ALT_LEFT, Modifiers::SHIFT_RIGHT),
            ),
            (
                both(Modifiers::CONTROL_LEFT, Modifiers::ALT_LEFT),
                Modifiers::CONTROL_LEFT,
                ModKey::Super,
                both(Modifiers::ALT_LEFT, Modifiers::META_LEFT),
            ),
            (
                both(Modifiers::CONTROL_LEFT, Modifiers::ALT_LEFT),
                both(Modifiers::CONTROL, Modifiers::ALT),
                ModKey::Super,
                Modifiers::META_LEFT,
            ),
        ];
        for (held, hold, mod_key, chord) in rows {
            assert_eq!(
                with_mod_for_hold(held, hold, mod_key),
                chord,
                "{held} held by {hold}, Mod {mod_key:?}"
            );
        }
    }

    /// Only the overview looks its keys up with `Mod`: a switch has no chord, nor does nothing open.
    #[test]
    fn only_a_live_overview_has_a_chord() {
        let base = Instant::now();
        let mut session = with_overview();
        assert_eq!(session.overview_chord(Modifiers::ALT_LEFT), None, "nothing open");
        session.on_key(down(KeyCode::KeyQ, Modifiers::CONTROL_LEFT, base));
        assert_eq!(session.overview_chord(Modifiers::CONTROL_LEFT), None, "a switch");

        let session = opened_overview(base);
        assert_eq!(
            session.overview_chord(both(Modifiers::ALT_RIGHT, Modifiers::SHIFT_LEFT)),
            Some(both(Modifiers::ALT_RIGHT, Modifiers::SHIFT_LEFT))
        );
    }

    fn bound(key: &str, action: Action) -> Bind {
        Bind {
            key: key.into(),
            action,
            repeat: false,
        }
    }

    fn parse(spec: &str) -> Option<(Modifiers, KeyCode)> {
        match spec {
            "Alt+Slash" => Some((Modifiers::ALT, KeyCode::Slash)),
            "Ctrl+Slash" => Some((Modifiers::CONTROL, KeyCode::Slash)),
            _ => None,
        }
    }

    /// Each bind decides both its key and what holds it, and every one of them opens it.
    #[test]
    fn every_toggle_overview_bind_opens_the_overview() {
        let binds = [
            bound("Alt+J", Action::FocusColumnLeft),
            bound("Alt+Slash", Action::ToggleOverview),
            bound("Ctrl+Slash", Action::ToggleOverview),
            bound("Hyper+Nonsense", Action::ToggleOverview),
        ];
        let keys = overview_keys(&binds, ModKey::Alt, parse);
        assert_eq!(keys, [
            SwitchKeys {
                opens: Opens::Overview { mod_key: ModKey::Alt },
                forward: KeyCode::Slash,
                hold: Modifiers::ALT,
            },
            SwitchKeys {
                opens: Opens::Overview { mod_key: ModKey::Alt },
                forward: KeyCode::Slash,
                hold: Modifiers::CONTROL,
            },
        ]);
        assert_eq!(
            overview_keys(&binds[..1], ModKey::Alt, parse),
            [],
            "no bind, no keys"
        );

        let base = Instant::now();
        let mut session = SwitchSession::default();
        session.set_keys(keys);
        let second = session.on_key(down(KeyCode::Slash, Modifiers::CONTROL_LEFT, base));
        assert_eq!(
            second,
            Verdict::eat_with(overview(OverviewSignal::Open(SessionId(1))))
        );
        assert_eq!(
            session.on_key(down(KeyCode::Slash, Modifiers::CONTROL_LEFT, at(base, 30))),
            Verdict::eat(),
            "its trigger again"
        );
        let released = flags(Modifiers::empty(), at(base, 60));
        assert_eq!(
            session.on_key(released),
            Verdict::pass_with(overview(OverviewSignal::Close)),
            "held by its own modifier"
        );
    }
}
