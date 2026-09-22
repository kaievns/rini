use std::ffi::c_void;

use objc2_core_foundation::{
    CFMachPort, CFRetained, CFRunLoop, CFRunLoopMode, CFRunLoopSource, kCFRunLoopCommonModes,
};
use objc2_core_graphics::{
    CGEvent, CGEventMask, CGEventTapLocation as CGTapLoc, CGEventTapOptions as CGTapOpt,
    CGEventTapPlacement as CGTapPlace, CGEventTapProxy, CGEventType,
};
use tracing::{debug, warn};

pub type TapCallback = Option<
    unsafe extern "C-unwind" fn(
        CGEventTapProxy,
        CGEventType,
        core::ptr::NonNull<CGEvent>,
        *mut c_void,
    ) -> *mut CGEvent,
>;

pub type TapDisabledCallback = Option<unsafe extern "C-unwind" fn(*mut c_void)>;
pub type TapInvalidatedCallback = Option<unsafe extern "C-unwind" fn(*mut c_void)>;

struct TrampolineCtx {
    callback: TapCallback,
    original_user_info: *mut c_void,
    original_drop: Option<unsafe fn(*mut c_void)>,
    disabled_callback: TapDisabledCallback,
    invalidated_callback: TapInvalidatedCallback,
    port_ptr: Option<core::ptr::NonNull<CFMachPort>>,
}

extern "C-unwind" fn port_invalidated(_port: *mut CFMachPort, user_info: *mut c_void) {
    if user_info.is_null() {
        return;
    }

    let ctx = unsafe { &*(user_info as *const TrampolineCtx) };
    warn!("Event tap Mach port was invalidated; scheduling tap recreation");
    if let Some(callback) = ctx.invalidated_callback {
        unsafe { callback(ctx.original_user_info) };
    }
}

extern "C-unwind" fn trampoline_callback(
    proxy: CGEventTapProxy,
    etype: CGEventType,
    event_ref: core::ptr::NonNull<CGEvent>,
    user_info: *mut c_void,
) -> *mut CGEvent {
    if user_info.is_null() {
        return event_ref.as_ptr();
    }

    let ctx = unsafe { &*(user_info as *const TrampolineCtx) };

    // kCGEventTapDisabledByTimeout (-2) & kCGEventTapDisabledByUserInput (-1)
    let ety = etype.0 as i32;
    if ety == -1 || ety == -2 {
        // Never re-enabled from the callback: that re-inserts a stalled tap and freezes all input
        // until reboot. The owner decides through [`ReEnableGovernor`]. See "A revoked
        // screen-recording grant froze all input" in docs/permissions-and-the-launch-agent.md.
        let reason = if ety == -2 { "timeout" } else { "user input" };
        warn!(reason, "Event tap was disabled; deferring to the owner's governor");
        if let Some(callback) = ctx.disabled_callback {
            unsafe { callback(ctx.original_user_info) };
        }
        return event_ref.as_ptr();
    }

    if let Some(orig_cb) = ctx.callback {
        return unsafe { orig_cb(proxy, etype, event_ref, ctx.original_user_info) };
    }

    event_ref.as_ptr()
}

unsafe fn trampoline_drop(ptr: *mut c_void) {
    if ptr.is_null() {
        return;
    }

    let ctx: Box<TrampolineCtx> = unsafe { Box::from_raw(ptr as *mut TrampolineCtx) };
    if let Some(dropper) = ctx.original_drop {
        if !ctx.original_user_info.is_null() {
            unsafe { dropper(ctx.original_user_info) };
        }
    }
}

pub struct EventTap {
    port: CFRetained<CFMachPort>,
    source: CFRetained<CFRunLoopSource>,
    user_info: *mut c_void,
    drop_ctx: Option<unsafe fn(*mut c_void)>,
}

/// Whether a disabled tap may be re-armed now or must sit out a cooldown. One disable is routine
/// (wake from sleep); a burst means the servicing thread is stuck and re-arming would re-freeze
/// input. Mechanism in docs/permissions-and-the-launch-agent.md.
pub struct ReEnableGovernor {
    disables: std::collections::VecDeque<std::time::Instant>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ReEnableDecision {
    Now,
    After(std::time::Duration),
}

/// Disables this frequent mean the tap is stalled, not unlucky. Three within the window requires
/// three consecutive timeout rounds, which ordinary load has never produced.
const DISABLE_BURST_LIMIT: usize = 3;
const DISABLE_BURST_WINDOW: std::time::Duration = std::time::Duration::from_secs(30);
/// Long enough that a stalled system stays usable (input flows while the tap is down), short
/// enough that hotkeys returning is not an event.
const REENABLE_COOLDOWN: std::time::Duration = std::time::Duration::from_secs(10);

impl Default for ReEnableGovernor {
    fn default() -> Self {
        Self::new()
    }
}

impl ReEnableGovernor {
    pub fn new() -> Self {
        Self { disables: std::collections::VecDeque::new() }
    }

    /// Records a disable at `now` and decides how to respond.
    pub fn on_disabled(&mut self, now: std::time::Instant) -> ReEnableDecision {
        self.disables.push_back(now);
        while let Some(&oldest) = self.disables.front() {
            if now.duration_since(oldest) > DISABLE_BURST_WINDOW {
                self.disables.pop_front();
            } else {
                break;
            }
        }
        if self.disables.len() >= DISABLE_BURST_LIMIT {
            ReEnableDecision::After(REENABLE_COOLDOWN)
        } else {
            ReEnableDecision::Now
        }
    }
}

/// A recovery message from a tap's own callbacks, on the tap's thread.
///
/// One type for both taps: the session tap and the HID gesture tap had an identical copy each,
/// alongside an identical forty-line handler for it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Recovery {
    /// The OS disabled the tap (timeout or user input); the governor decides when to re-arm.
    TapDisabled(u64),
    /// A deferred re-arm's cooldown ran out.
    CooldownElapsed(u64),
    /// The Mach port died, so there is nothing left to re-enable.
    TapInvalidated(u64),
}

/// What a tap thread should do about a `Recovery`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Recovered {
    /// Re-arm this generation now.
    ReArm(u64),
    /// Leave the tap down for `wait`, then re-arm. Input keeps flowing without it meanwhile.
    StandDown { generation: u64, wait: std::time::Duration },
    /// Build a new tap; this one's port is gone.
    Rebuild(u64),
    /// A message about a tap that has already been replaced. Doing anything would re-arm a dead
    /// generation, which is how a stale tap came back and started swallowing events again.
    Ignore,
}

/// Decide what a recovery message means, given which generation is live now.
///
/// Pure so the generation rules can be exercised without a tap: `ReEnableGovernor` was already
/// extracted and tested, but the part that decides whether a message is even about the current tap
/// was written twice inline and tested neither time.
pub fn on_recovery(
    recovery: Recovery,
    live_generation: u64,
    governor: &mut ReEnableGovernor,
    now: std::time::Instant,
) -> Recovered {
    match recovery {
        Recovery::TapDisabled(generation) => {
            if generation != live_generation {
                return Recovered::Ignore;
            }
            match governor.on_disabled(now) {
                ReEnableDecision::Now => Recovered::ReArm(generation),
                ReEnableDecision::After(wait) => Recovered::StandDown { generation, wait },
            }
        }
        Recovery::CooldownElapsed(generation) => {
            if generation == live_generation {
                Recovered::ReArm(generation)
            } else {
                Recovered::Ignore
            }
        }
        // Not generation-checked on purpose: an invalidated port cannot be re-enabled, so the tap
        // has to be rebuilt whichever generation reported it.
        Recovery::TapInvalidated(generation) => Recovered::Rebuild(generation),
    }
}

impl EventTap {
    unsafe fn create(
        location: CGTapLoc,
        options: CGTapOpt,
        mask: CGEventMask,
        callback: TapCallback,
        user_info: *mut c_void,
        drop_ctx: Option<unsafe fn(*mut c_void)>,
        disabled_callback: TapDisabledCallback,
        invalidated_callback: TapInvalidatedCallback,
    ) -> Option<Self> {
        let tramp = Box::new(TrampolineCtx {
            callback,
            original_user_info: user_info,
            original_drop: drop_ctx,
            disabled_callback,
            invalidated_callback,
            port_ptr: None,
        });
        let tramp_ptr = Box::into_raw(tramp) as *mut c_void;

        let port = unsafe {
            CGEvent::tap_create(
                location,
                CGTapPlace::HeadInsertEventTap,
                options,
                mask,
                Some(trampoline_callback),
                tramp_ptr,
            )?
        };

        let source = CFMachPort::new_run_loop_source(None, Some(&port), 0)?;
        if let Some(rl) = CFRunLoop::current() {
            debug!(
                "EventTap::new_at_location_with_options: CFRunLoop::current() returned a run loop; adding source to common modes"
            );
            let mode: &CFRunLoopMode = unsafe {
                kCFRunLoopCommonModes.expect("kCFRunLoopCommonModes should be available on macOS")
            };
            rl.add_source(Some(&source), Some(mode));
        } else {
            debug!(
                "EventTap::new_at_location_with_options: CFRunLoop::current() returned None; run loop not present"
            );
        }
        CGEvent::tap_enable(&port, true);

        let event_tap = Self {
            port,
            source,
            user_info: tramp_ptr,
            drop_ctx: Some(trampoline_drop),
        };

        unsafe {
            let tramp_ctx = &mut *(tramp_ptr as *mut TrampolineCtx);
            tramp_ctx.port_ptr = Some(core::ptr::NonNull::from(&*event_tap.port));
            event_tap.port.set_invalidation_call_back(Some(port_invalidated));
        }

        Some(event_tap)
    }

    pub unsafe fn new_at_location_with_options(
        location: CGTapLoc,
        options: CGTapOpt,
        mask: CGEventMask,
        callback: TapCallback,
        user_info: *mut c_void,
        drop_ctx: Option<unsafe fn(*mut c_void)>,
    ) -> Option<Self> {
        unsafe {
            Self::create(
                location, options, mask, callback, user_info, drop_ctx, None, None,
            )
        }
    }

    /// Creates an event tap at `location` that reports disables and Mach-port invalidations to
    /// its owner, which re-enables through [`EventTap::re_enable`] under its own policy.
    pub unsafe fn new_at_location_with_options_and_recovery_callbacks(
        location: CGTapLoc,
        options: CGTapOpt,
        mask: CGEventMask,
        callback: TapCallback,
        user_info: *mut c_void,
        drop_ctx: Option<unsafe fn(*mut c_void)>,
        disabled_callback: TapDisabledCallback,
        invalidated_callback: TapInvalidatedCallback,
    ) -> Option<Self> {
        unsafe {
            Self::create(
                location,
                options,
                mask,
                callback,
                user_info,
                drop_ctx,
                disabled_callback,
                invalidated_callback,
            )
        }
    }

    pub unsafe fn new_with_options(
        options: CGTapOpt,
        mask: CGEventMask,
        callback: TapCallback,
        user_info: *mut c_void,
        drop_ctx: Option<unsafe fn(*mut c_void)>,
    ) -> Option<Self> {
        unsafe {
            Self::new_at_location_with_options(
                CGTapLoc::SessionEventTap,
                options,
                mask,
                callback,
                user_info,
                drop_ctx,
            )
        }
    }

    /// Creates a session event tap that reports disables (`disabled_callback`) and Mach-port
    /// invalidations (`invalidated_callback`) to its owner instead of recovering in the callback.
    pub unsafe fn new_with_options_and_recovery_callbacks(
        options: CGTapOpt,
        mask: CGEventMask,
        callback: TapCallback,
        user_info: *mut c_void,
        drop_ctx: Option<unsafe fn(*mut c_void)>,
        disabled_callback: TapDisabledCallback,
        invalidated_callback: TapInvalidatedCallback,
    ) -> Option<Self> {
        unsafe {
            Self::new_at_location_with_options_and_recovery_callbacks(
                CGTapLoc::SessionEventTap,
                options,
                mask,
                callback,
                user_info,
                drop_ctx,
                disabled_callback,
                invalidated_callback,
            )
        }
    }

    /// Re-arms a tap the OS disabled. `false` means the port is dead and the tap
    /// has to be recreated.
    pub fn re_enable(&self) -> bool {
        CGEvent::tap_enable(&self.port, true);
        CGEvent::tap_is_enabled(&self.port)
    }

    pub unsafe fn new_listen_only(
        mask: CGEventMask,
        callback: TapCallback,
        user_info: *mut c_void,
        drop_ctx: Option<unsafe fn(*mut c_void)>,
    ) -> Option<Self> {
        unsafe { Self::new_with_options(CGTapOpt::ListenOnly, mask, callback, user_info, drop_ctx) }
    }

    pub unsafe fn new_at_location_listen_only(
        location: CGTapLoc,
        mask: CGEventMask,
        callback: TapCallback,
        user_info: *mut c_void,
        drop_ctx: Option<unsafe fn(*mut c_void)>,
    ) -> Option<Self> {
        unsafe {
            Self::new_at_location_with_options(
                location,
                CGTapOpt::ListenOnly,
                mask,
                callback,
                user_info,
                drop_ctx,
            )
        }
    }

    pub fn set_enabled(&self, enabled: bool) {
        CGEvent::tap_enable(&self.port, enabled);
    }
}

impl Drop for EventTap {
    fn drop(&mut self) {
        if self.port.is_valid() {
            // Intentional teardown/replacement must not be mistaken for an
            // unexpected Mach-port failure by the event-driven recovery path.
            unsafe { self.port.set_invalidation_call_back(None) };
            CGEvent::tap_enable(&self.port, false);
        }
        if let Some(rl) = CFRunLoop::current() {
            rl.remove_source(Some(&self.source), unsafe { kCFRunLoopCommonModes });
        }
        if let Some(dropper) = self.drop_ctx {
            unsafe { dropper(self.user_info) };
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;

    #[test]
    fn an_isolated_disable_re_enables_immediately() {
        // The routine case: wake from sleep, a one-off stall. Waiting here would just be ten
        // seconds of dead hotkeys for nothing.
        let mut governor = ReEnableGovernor::new();
        assert_eq!(governor.on_disabled(Instant::now()), ReEnableDecision::Now);
    }

    #[test]
    fn a_burst_of_disables_backs_off() {
        // The measured freeze: a revoked screen-recording grant stalls the callback, the OS
        // disables the tap, and each instant re-enable re-freezes every input device for another
        // timeout round. The third disable in the window has to stand down instead.
        let mut governor = ReEnableGovernor::new();
        let start = Instant::now();
        assert_eq!(governor.on_disabled(start), ReEnableDecision::Now);
        assert_eq!(governor.on_disabled(start + Duration::from_secs(2)), ReEnableDecision::Now);
        assert_eq!(
            governor.on_disabled(start + Duration::from_secs(4)),
            ReEnableDecision::After(REENABLE_COOLDOWN)
        );
    }

    #[test]
    fn the_burst_keeps_backing_off_while_the_stall_lasts() {
        // While the underlying stall persists, every re-arm gets disabled again; each of those
        // must keep deferring, or the freeze returns at full duty cycle.
        let mut governor = ReEnableGovernor::new();
        let start = Instant::now();
        governor.on_disabled(start);
        governor.on_disabled(start + Duration::from_secs(2));
        governor.on_disabled(start + Duration::from_secs(4));
        assert_eq!(
            governor.on_disabled(start + Duration::from_secs(15)),
            ReEnableDecision::After(REENABLE_COOLDOWN)
        );
    }

    #[test]
    fn disables_spread_beyond_the_window_stay_immediate() {
        // One disable every few minutes is a busy machine, not a stalled tap.
        let mut governor = ReEnableGovernor::new();
        let start = Instant::now();
        for minutes in [0u64, 3, 6, 9] {
            assert_eq!(
                governor.on_disabled(start + Duration::from_secs(minutes * 60)),
                ReEnableDecision::Now
            );
        }
    }

    #[test]
    fn the_governor_recovers_after_a_quiet_spell() {
        // Once the stall clears and the window drains, the tap goes back to immediate re-enables.
        let mut governor = ReEnableGovernor::new();
        let start = Instant::now();
        governor.on_disabled(start);
        governor.on_disabled(start + Duration::from_secs(1));
        governor.on_disabled(start + Duration::from_secs(2));
        assert_eq!(
            governor.on_disabled(start + Duration::from_secs(120)),
            ReEnableDecision::Now
        );
    }
}

#[cfg(test)]
mod recovery_tests {
    use std::time::{Duration, Instant};

    use super::{ReEnableGovernor, Recovered, Recovery, on_recovery};

    fn decide(recovery: Recovery, live: u64, governor: &mut ReEnableGovernor) -> Recovered {
        on_recovery(recovery, live, governor, Instant::now())
    }

    #[test]
    fn a_disable_of_the_live_tap_re_arms_it() {
        let mut governor = ReEnableGovernor::new();
        assert_eq!(decide(Recovery::TapDisabled(7), 7, &mut governor), Recovered::ReArm(7));
    }

    /// The rule that kept a replaced tap from coming back. A message about an old generation must
    /// not re-arm anything: that tap is gone, and re-enabling it puts a second tap in the delivery
    /// path swallowing events.
    #[test]
    fn a_disable_of_a_replaced_tap_is_ignored() {
        let mut governor = ReEnableGovernor::new();
        assert_eq!(decide(Recovery::TapDisabled(6), 7, &mut governor), Recovered::Ignore);
        assert_eq!(decide(Recovery::CooldownElapsed(6), 7, &mut governor), Recovered::Ignore);
    }

    #[test]
    fn a_stale_disable_does_not_count_against_the_burst_limit() {
        let mut governor = ReEnableGovernor::new();
        for _ in 0..5 {
            assert_eq!(decide(Recovery::TapDisabled(1), 9, &mut governor), Recovered::Ignore);
        }
        assert_eq!(
            decide(Recovery::TapDisabled(9), 9, &mut governor),
            Recovered::ReArm(9),
            "the live tap's first disable is still its first"
        );
    }

    /// Three disables inside the window mean the tap is stalled rather than unlucky, and it stands
    /// down so macOS keeps delivering input without it.
    #[test]
    fn a_burst_of_disables_stands_the_tap_down() {
        let mut governor = ReEnableGovernor::new();
        let now = Instant::now();
        assert_eq!(on_recovery(Recovery::TapDisabled(1), 1, &mut governor, now), Recovered::ReArm(1));
        assert_eq!(
            on_recovery(Recovery::TapDisabled(1), 1, &mut governor, now + Duration::from_secs(1)),
            Recovered::ReArm(1)
        );
        let third =
            on_recovery(Recovery::TapDisabled(1), 1, &mut governor, now + Duration::from_secs(2));
        match third {
            Recovered::StandDown { generation, wait } => {
                assert_eq!(generation, 1);
                assert!(wait >= Duration::from_secs(1), "a stand-down has to actually wait");
            }
            other => panic!("expected a stand-down, got {other:?}"),
        }
    }

    #[test]
    fn a_cooldown_elapsing_on_the_live_tap_re_arms_it() {
        let mut governor = ReEnableGovernor::new();
        assert_eq!(decide(Recovery::CooldownElapsed(4), 4, &mut governor), Recovered::ReArm(4));
    }

    /// An invalidated port is NOT generation-checked: it cannot be re-enabled at all, so whichever
    /// generation noticed, a new tap has to be built.
    #[test]
    fn an_invalidated_port_is_rebuilt_whatever_generation_reported_it() {
        let mut governor = ReEnableGovernor::new();
        assert_eq!(decide(Recovery::TapInvalidated(2), 9, &mut governor), Recovered::Rebuild(2));
    }
}
