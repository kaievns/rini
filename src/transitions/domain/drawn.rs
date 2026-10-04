//! What waits for the screen: the effects of the coordinator's commands that must not happen before
//! the batch of render commands carrying the overlay's change is drawn, numbered by batch and
//! released as the renderer reports each one committed. See "One engine" in
//! `src/transitions/docs/coordination.md`.

use rini_core::ids::WindowId;
use rini_geometry::space::ScreenRect;

use crate::transitions::domain::lifecycle::Generation;

/// An effect waiting for a batch to be drawn.
#[derive(Clone, Debug, PartialEq)]
pub enum Owed {
    /// Real frames written: a window moved before the overlay covering it is up jumps to its end.
    Write(Vec<(WindowId, ScreenRect)>),
    /// The bar pictured again, which must not find the overlay over it.
    Settle,
    /// The quiet after `Generation`'s lift, which counts from the overlay coming down.
    Quiet(Generation),
}

/// The batches handed to the renderer, the last it committed, and what waits for one.
#[derive(Debug, Default)]
pub struct Drawn {
    next: u64,
    committed: u64,
    owed: Vec<(u64, Owed)>,
}

impl Drawn {
    /// The number of the next batch, one past the last.
    pub fn batch(&mut self) -> u64 {
        self.next += 1;
        self.next
    }

    /// `owed` waits for batch `after`; `Some` when that is committed already, to carry out now.
    pub fn owe(&mut self, after: u64, owed: Owed) -> Option<Owed> {
        if after <= self.committed {
            return Some(owed);
        }
        self.owed.push((after, owed));
        None
    }

    /// The renderer committed every batch up to `seq`: what that releases, in the order it was owed.
    pub fn committed(&mut self, seq: u64) -> Vec<Owed> {
        self.committed = self.committed.max(seq);
        let committed = self.committed;
        let (due, waiting): (Vec<_>, Vec<_>) =
            std::mem::take(&mut self.owed).into_iter().partition(|(after, _)| *after <= committed);
        self.owed = waiting;
        due.into_iter().map(|(_, owed)| owed).collect()
    }

    /// Whether every batch handed over is committed.
    pub fn caught_up(&self) -> bool {
        self.committed >= self.next
    }

    /// The flight is abandoned: nothing it owes is carried out, since the lifecycle's `Abandon` writes
    /// the final frames itself.
    pub fn abandon(&mut self) {
        self.owed.clear();
    }

    /// A new hold: the last lift's `Settle` and quiet are dropped, as on one thread they were carried
    /// out before it, and the writes owed stay.
    pub fn begun(&mut self) {
        self.owed.retain(|(_, owed)| matches!(owed, Owed::Write(_)));
    }
}

#[cfg(test)]
mod tests {
    use rini_geometry::space::{Point, Size};

    use super::*;

    fn write(idx: u32) -> Owed {
        let frame = ScreenRect::new(Point::new(4.0 + f64::from(idx), 32.0), Size::new(859.0, 1081.0));
        Owed::Write(vec![(WindowId::new(7, idx), frame)])
    }

    #[test]
    fn a_write_owed_to_an_uncommitted_batch_waits_for_it() {
        let mut drawn = Drawn::default();
        let shown = drawn.batch();
        assert_eq!(drawn.owe(shown, write(1)), None);
        assert!(drawn.committed(shown - 1).is_empty(), "an earlier batch releases nothing of it");
        assert_eq!(drawn.committed(shown), vec![write(1)]);
        assert!(drawn.committed(shown).is_empty(), "released once");
    }

    #[test]
    fn a_commit_releases_what_it_covers_in_the_order_owed() {
        let mut drawn = Drawn::default();
        let (first, second, third) = (drawn.batch(), drawn.batch(), drawn.batch());
        assert_eq!(drawn.owe(second, Owed::Settle), None);
        assert_eq!(drawn.owe(first, write(1)), None);
        assert_eq!(drawn.owe(third, Owed::Quiet(Generation(3))), None);
        assert_eq!(drawn.owe(second, write(2)), None);
        assert_eq!(drawn.committed(second), vec![Owed::Settle, write(1), write(2)], "a drain acknowledges its highest");
        assert_eq!(drawn.committed(third), vec![Owed::Quiet(Generation(3))]);
    }

    #[test]
    fn what_is_owed_to_a_committed_batch_goes_at_once() {
        let mut drawn = Drawn::default();
        assert_eq!(drawn.owe(0, write(1)), Some(write(1)), "nothing shown yet");
        let shown = drawn.batch();
        drawn.committed(shown);
        assert_eq!(drawn.owe(shown, write(2)), Some(write(2)));
        assert_eq!(drawn.owe(shown, Owed::Settle), Some(Owed::Settle));
    }

    #[test]
    fn caught_up_only_when_every_batch_sent_is_committed() {
        let mut drawn = Drawn::default();
        assert!(drawn.caught_up(), "nothing sent");
        let (first, second) = (drawn.batch(), drawn.batch());
        assert!(!drawn.caught_up());
        drawn.committed(first);
        assert!(!drawn.caught_up());
        drawn.committed(second);
        assert!(drawn.caught_up());
    }

    #[test]
    fn an_abandoned_flight_owes_nothing() {
        let mut drawn = Drawn::default();
        let shown = drawn.batch();
        drawn.owe(shown, write(1));
        drawn.owe(shown, Owed::Quiet(Generation(1)));
        drawn.abandon();
        assert!(drawn.committed(shown).is_empty());
        assert!(drawn.caught_up(), "the batch still counts as drawn");
    }

    #[test]
    fn a_new_hold_drops_what_the_last_lift_owed_and_keeps_its_writes() {
        let mut drawn = Drawn::default();
        let lifted = drawn.batch();
        drawn.owe(lifted, write(1));
        drawn.owe(lifted, Owed::Quiet(Generation(1)));
        drawn.owe(lifted, Owed::Settle);
        drawn.begun();
        assert_eq!(drawn.committed(lifted), vec![write(1)]);
    }
}
