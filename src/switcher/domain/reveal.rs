//! When the popup appears: only once a switch has been HELD, never on a quick combo.
//!
//! A quick tap — press the chord, let go — steps to the previous window and nothing else. Drawing the
//! popup for it flashes a panel the user never meant to look at. So the first draw of a switch waits
//! `HOLD_TO_REVEAL`, and a switch that commits or cancels inside that window is never drawn at all.
//!
//! The switch itself does not wait. The selection and the commit are decided on the reactor side the
//! moment the keys arrive; only the picture of them is held back.

use std::time::{Duration, Instant};

/// How long a switch has to stay open before the popup is drawn.
///
/// A judgement, not a measurement: long enough that the modifier's release in a quick combo lands
/// before it, short enough that a deliberate hold does not feel like waiting.
pub const HOLD_TO_REVEAL: Duration = Duration::from_millis(200);

/// What to do with a draw the reactor asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Draw {
    /// Draw it now.
    Now,
    /// Keep it, and draw it at `due` if the switch is still open then.
    Later { due: Instant },
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum State {
    #[default]
    Hidden,
    Pending {
        due: Instant,
    },
    Shown,
}

/// Whether the popup is hidden, waiting out the hold, or up.
#[derive(Debug, Default)]
pub struct Reveal {
    state: State,
}

impl Reveal {
    /// The reactor asked for a draw at `now`.
    ///
    /// The first one of a switch waits out the hold. A SECOND one while still waiting draws at once:
    /// the reactor only redraws for a step, and a step is the user walking the list, which is as sure a
    /// sign of a hold as the modifier staying down.
    pub fn on_show(&mut self, now: Instant) -> Draw {
        match self.state {
            State::Hidden => {
                let due = now + HOLD_TO_REVEAL;
                self.state = State::Pending { due };
                Draw::Later { due }
            }
            State::Pending { .. } | State::Shown => {
                self.state = State::Shown;
                Draw::Now
            }
        }
    }

    /// When a waiting draw is due, if one is.
    pub fn due(&self) -> Option<Instant> {
        match self.state {
            State::Pending { due } => Some(due),
            State::Hidden | State::Shown => None,
        }
    }

    /// The clock reached `now`. True when the waiting draw should happen now.
    pub fn on_tick(&mut self, now: Instant) -> bool {
        match self.state {
            State::Pending { due } if now >= due => {
                self.state = State::Shown;
                true
            }
            _ => false,
        }
    }

    /// The switch ended. True when there is a popup up to take down.
    pub fn on_hide(&mut self) -> bool {
        std::mem::take(&mut self.state) == State::Shown
    }

    pub fn is_shown(&self) -> bool {
        self.state == State::Shown
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(base: Instant, millis: u64) -> Instant {
        base + Duration::from_millis(millis)
    }

    /// The reported bug: a quick combo showed the popup. Opened and released inside the hold, nothing
    /// is ever drawn.
    #[test]
    fn a_quick_combo_never_draws() {
        let base = Instant::now();
        let mut reveal = Reveal::default();

        assert_eq!(reveal.on_show(base), Draw::Later { due: at(base, 200) });
        assert!(!reveal.on_tick(at(base, 90)), "not yet");
        assert!(
            !reveal.on_hide(),
            "released before the hold: nothing to take down"
        );
        assert!(
            !reveal.on_tick(at(base, 250)),
            "and a deadline arriving after the release draws nothing"
        );
        assert!(!reveal.is_shown());
    }

    /// Keep holding the modifier and the popup appears once the hold is up.
    #[test]
    fn a_held_switch_draws_once_the_hold_is_up() {
        let base = Instant::now();
        let mut reveal = Reveal::default();
        reveal.on_show(base);

        assert!(reveal.on_tick(at(base, 200)));
        assert!(reveal.is_shown());
        assert!(!reveal.on_tick(at(base, 400)), "drawn once, not on every tick");
        assert!(reveal.on_hide(), "and the release takes it down");
    }

    /// A step inside the hold is the user walking the list, so the popup comes up at once rather than
    /// making them step blind.
    #[test]
    fn a_step_during_the_hold_draws_at_once() {
        let base = Instant::now();
        let mut reveal = Reveal::default();
        reveal.on_show(base);

        assert_eq!(reveal.on_show(at(base, 60)), Draw::Now);
        assert_eq!(reveal.due(), None, "nothing left waiting");
    }

    #[test]
    fn once_up_every_draw_is_immediate() {
        let base = Instant::now();
        let mut reveal = Reveal::default();
        reveal.on_show(base);
        reveal.on_tick(at(base, 200));

        assert_eq!(reveal.on_show(at(base, 300)), Draw::Now);
    }

    /// Each switch waits out its own hold: a previous switch that was shown does not let the next
    /// quick combo draw.
    #[test]
    fn every_switch_starts_hidden() {
        let base = Instant::now();
        let mut reveal = Reveal::default();
        reveal.on_show(base);
        reveal.on_tick(at(base, 200));
        reveal.on_hide();

        assert!(matches!(reveal.on_show(at(base, 1000)), Draw::Later { .. }));
    }
}
