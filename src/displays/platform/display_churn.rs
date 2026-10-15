//! Whether a display reconfiguration is under way, for readers off the spaces actor's thread.
use std::sync::atomic::{AtomicBool, Ordering};

static DISPLAY_CHURN_ACTIVE: AtomicBool = AtomicBool::new(false);

pub fn begin() {
    DISPLAY_CHURN_ACTIVE.store(true, Ordering::SeqCst);
}

pub fn end() {
    DISPLAY_CHURN_ACTIVE.store(false, Ordering::SeqCst);
}

pub fn is_active() -> bool {
    DISPLAY_CHURN_ACTIVE.load(Ordering::SeqCst)
}

#[cfg(test)]
static HELD_FOR_TEST: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Held by every test that sets the flag: one such test runs at a time, and the flag is clear
/// when it ends, pass or fail.
#[cfg(test)]
pub(crate) struct HeldForTest {
    _lock: std::sync::MutexGuard<'static, ()>,
}

#[cfg(test)]
pub(crate) fn hold_for_test() -> HeldForTest {
    HeldForTest {
        _lock: HELD_FOR_TEST.lock().unwrap_or_else(|poisoned| poisoned.into_inner()),
    }
}

#[cfg(test)]
impl Drop for HeldForTest {
    fn drop(&mut self) {
        end();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn begin_end_toggles_global_state() {
        let _held = hold_for_test();
        begin();
        assert!(is_active());
        end();
        assert!(!is_active());
    }

    #[test]
    fn a_test_holding_the_flag_leaves_it_clear() {
        {
            let _held = hold_for_test();
            begin();
            assert!(is_active());
        }
        let _held = hold_for_test();
        assert!(!is_active());
    }
}
