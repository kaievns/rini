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
mod tests {
    use super::*;

    #[test]
    fn begin_end_toggles_global_state() {
        begin();
        assert!(is_active());
        end();
        assert!(!is_active());
    }
}
