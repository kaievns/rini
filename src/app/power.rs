//! Low Power Mode, read once at startup and kept current by the application from
//! `NSProcessInfoPowerStateDidChangeNotification`. A flight is not worth its cost on battery saver.
use std::sync::atomic::{AtomicBool, Ordering};

use objc2_foundation::NSProcessInfo;

static LOW_POWER_MODE: AtomicBool = AtomicBool::new(false);

pub fn is_low_power_mode_enabled() -> bool {
    LOW_POWER_MODE.load(Ordering::Relaxed)
}

pub fn set_low_power_mode_state(new_state: bool) -> bool {
    LOW_POWER_MODE.swap(new_state, Ordering::Relaxed)
}

pub fn init_power_state() {
    let process_info = NSProcessInfo::processInfo();
    let initial_state = process_info.isLowPowerModeEnabled();
    LOW_POWER_MODE.store(initial_state, Ordering::Relaxed);
}
