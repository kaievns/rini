//! Which applications ask for attention. macOS asks per application, not per window: an app's
//! `requestUserAttention:` bounces its Dock icon until the app is activated or cancels it, and
//! LaunchServices keeps the ask as the app's `LSWantsAttention` (`src/windows/docs/attention.md`).

use std::collections::BTreeSet;

use rini_core::ids::pid_t;

/// The applications asking for attention now.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Attention {
    asking: BTreeSet<pid_t>,
}

impl Attention {
    pub fn of(asking: impl IntoIterator<Item = pid_t>) -> Attention { Attention { asking: asking.into_iter().collect() } }

    /// Whether the application `pid` asks for attention.
    pub fn asks(&self, pid: pid_t) -> bool { self.asking.contains(&pid) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_application_asks_while_it_is_named() {
        let attention = Attention::of([42, 7, 42]);
        assert!(attention.asks(42) && attention.asks(7));
        assert!(!attention.asks(9));
        assert_eq!(attention, Attention::of([7, 42]), "the order it was read in is not part of it");
        assert!(!Attention::default().asks(42));
    }
}
