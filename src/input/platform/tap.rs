use std::ffi::c_void;
use std::sync::Arc;

use objc2_core_foundation::{
    CFMachPort, CFRetained, CFRunLoop, CFRunLoopMode, CFRunLoopSource, kCFRunLoopCommonModes,
};
use objc2_core_graphics::{
    CGEvent, CGEventMask, CGEventTapLocation as CGTapLoc, CGEventTapOptions as CGTapOpt,
    CGEventTapPlacement as CGTapPlace, CGEventTapProxy, CGEventType,
};
use parking_lot::Mutex;
use tracing::{debug, warn};

pub type TapCallback = Option<
    unsafe extern "C-unwind" fn(
        CGEventTapProxy,
        CGEventType,
        core::ptr::NonNull<CGEvent>,
        *mut c_void,
    ) -> *mut CGEvent,
>;

/// What a tap calls when the OS disables it, on the tap's thread.
pub type OnDisabled = Box<dyn Fn()>;
/// What a tap calls when its Mach port dies, on whichever thread Core Foundation invalidates it.
pub type OnInvalidated = Arc<dyn Fn() + Send + Sync>;

struct TrampolineCtx {
    callback: TapCallback,
    original_user_info: *mut c_void,
    original_drop: Option<unsafe fn(*mut c_void)>,
    on_disabled: OnDisabled,
}

/// Each live tap's port, by address, and what its invalidation calls. A tap port's context info is
/// Core Graphics' own record rather than the tap's refcon, so the callback finds its handler by port:
/// see "A dead tap is found by its port" in src/input/docs/README.md.
static ON_INVALIDATED: Mutex<Vec<(usize, OnInvalidated)>> = Mutex::new(Vec::new());

/// The handler `port`'s invalidation calls until the guard drops.
struct InvalidationWatch {
    port: usize,
}

impl InvalidationWatch {
    fn new(port: usize, on_invalidated: OnInvalidated) -> Self {
        ON_INVALIDATED.lock().push((port, on_invalidated));
        InvalidationWatch { port }
    }
}

impl Drop for InvalidationWatch {
    fn drop(&mut self) { ON_INVALIDATED.lock().retain(|(port, _)| *port != self.port); }
}

/// Reads neither the port nor `_info`: the port's address only picks the handler.
extern "C-unwind" fn port_invalidated(port: *mut CFMachPort, _info: *mut c_void) {
    let on_invalidated = ON_INVALIDATED
        .lock()
        .iter()
        .find(|(watched, _)| *watched == port as usize)
        .map(|(_, on_invalidated)| Arc::clone(on_invalidated));
    if let Some(on_invalidated) = on_invalidated {
        warn!("Event tap Mach port was invalidated; scheduling tap recreation");
        on_invalidated();
    }
}

/// # Safety
///
/// `user_info` is null or the live `TrampolineCtx` the tap was made with, and `event_ref` is the
/// event Core Graphics passes, valid for the call.
unsafe extern "C-unwind" fn trampoline_callback(
    proxy: CGEventTapProxy,
    etype: CGEventType,
    event_ref: core::ptr::NonNull<CGEvent>,
    user_info: *mut c_void,
) -> *mut CGEvent {
    if user_info.is_null() {
        return event_ref.as_ptr();
    }

    // SAFETY: non-null, so the caller guarantees it is this tap's live `TrampolineCtx`, freed only
    // by its `Drop` after the tap is disabled and its source is off the run loop this runs on.
    let ctx = unsafe { &*(user_info as *const TrampolineCtx) };

    // kCGEventTapDisabledByTimeout (-2) & kCGEventTapDisabledByUserInput (-1)
    let ety = etype.0 as i32;
    if ety == -1 || ety == -2 {
        // Never re-enabled from the callback: that re-inserts a stalled tap and freezes all input
        // until reboot. The owner decides through [`ReEnableGovernor`]. See "A revoked
        // screen-recording grant froze all input" in docs/permissions-and-the-launch-agent.md.
        let reason = if ety == -2 { "timeout" } else { "user input" };
        warn!(
            reason,
            "Event tap was disabled; deferring to the owner's governor"
        );
        (ctx.on_disabled)();
        return event_ref.as_ptr();
    }

    if let Some(orig_cb) = ctx.callback {
        // SAFETY: the owner's `user_info` is what its `callback` expects and stays valid until the
        // tap drops it, as `new_with_options_and_recovery_callbacks` requires.
        return unsafe { orig_cb(proxy, etype, event_ref, ctx.original_user_info) };
    }

    event_ref.as_ptr()
}

/// Frees a tap's `TrampolineCtx`, and the owner's `user_info` with it.
///
/// # Safety
///
/// `ptr` is the `TrampolineCtx` a tap was made with, not freed before, and no callback can reach it
/// again.
unsafe fn trampoline_drop(ptr: *mut c_void) {
    // SAFETY: `ptr` is the boxed `TrampolineCtx`, freed exactly once here, as the caller guarantees.
    let ctx: Box<TrampolineCtx> = unsafe { Box::from_raw(ptr as *mut TrampolineCtx) };
    if let Some(dropper) = ctx.original_drop
        && !ctx.original_user_info.is_null()
    {
        // SAFETY: `dropper` frees what the owner handed in, which only this tap held.
        unsafe { dropper(ctx.original_user_info) };
    }
}

/// The run-loop modes a tap's source is scheduled in.
fn common_modes() -> Option<&'static CFRunLoopMode> {
    // SAFETY: an immutable constant Core Foundation exports, set before any Rust code runs.
    unsafe { kCFRunLoopCommonModes }
}

/// A session event tap. Not `Send`: its source is on the run loop of the thread that made it, and
/// `Drop` takes it off the current one.
pub struct EventTap {
    /// First, so it is dropped before releasing the port and its source invalidates the port.
    _invalidation: InvalidationWatch,
    port: CFRetained<CFMachPort>,
    source: CFRetained<CFRunLoopSource>,
    trampoline: *mut c_void,
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
    fn default() -> Self { Self::new() }
}

impl ReEnableGovernor {
    pub fn new() -> Self {
        Self {
            disables: std::collections::VecDeque::new(),
        }
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

/// A recovery message from the session tap's own callbacks, on the tap's thread.
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
    StandDown {
        generation: u64,
        wait: std::time::Duration,
    },
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

/// Why a session tap could not be made.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TapError {
    /// `CGEventTapCreate` returned null.
    Create,
    /// The tap's Mach port gave no run-loop source.
    RunLoopSource,
}

impl std::fmt::Display for TapError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            TapError::Create => "CGEventTapCreate returned null",
            TapError::RunLoopSource => "the tap's Mach port gave no run-loop source",
        })
    }
}

/// Passes `ctx` to `create` as the pointer a port keeps for its callbacks, and frees it when
/// `create` fails. `create` must not leave the pointer anywhere on `Err`.
fn hand_to_port<T, P>(
    ctx: Box<T>,
    create: impl FnOnce(*mut c_void) -> Result<P, TapError>,
) -> Result<(P, *mut c_void), TapError> {
    let ptr = Box::into_raw(ctx) as *mut c_void;
    match create(ptr) {
        Ok(port) => Ok((port, ptr)),
        Err(error) => {
            // SAFETY: `ptr` is the `Box::into_raw` above, and `create` left it nowhere.
            drop(unsafe { Box::from_raw(ptr as *mut T) });
            Err(error)
        }
    }
}

impl EventTap {
    /// Creates a session event tap that reports disables (`on_disabled`) and Mach-port
    /// invalidations (`on_invalidated`) to its owner instead of recovering in the callback.
    /// On `Err` the tap never took `user_info`, so the caller still owns it.
    ///
    /// # Safety
    ///
    /// `user_info` is what `callback` and `drop_ctx` expect, and stays valid until the tap is
    /// dropped, which frees it with `drop_ctx`.
    pub unsafe fn new_with_options_and_recovery_callbacks(
        options: CGTapOpt,
        mask: CGEventMask,
        callback: TapCallback,
        user_info: *mut c_void,
        drop_ctx: Option<unsafe fn(*mut c_void)>,
        on_disabled: OnDisabled,
        on_invalidated: OnInvalidated,
    ) -> Result<Self, TapError> {
        let tramp = Box::new(TrampolineCtx {
            callback,
            original_user_info: user_info,
            original_drop: drop_ctx,
            on_disabled,
        });

        let ((port, source), trampoline) = hand_to_port(tramp, |tramp_ptr| {
            // SAFETY: `trampoline_callback` requires its refcon be this `TrampolineCtx`, which lives
            // until the tap's `Drop` has disabled it, and Core Graphics passes a valid event.
            let port = unsafe {
                CGEvent::tap_create(
                    CGTapLoc::SessionEventTap,
                    CGTapPlace::HeadInsertEventTap,
                    options,
                    mask,
                    Some(trampoline_callback),
                    tramp_ptr,
                )
            }
            .ok_or(TapError::Create)?;
            let source = CFMachPort::new_run_loop_source(None, Some(&port), 0)
                .ok_or(TapError::RunLoopSource)?;
            Ok((port, source))
        })?;
        if let Some(rl) = CFRunLoop::current() {
            debug!(
                "EventTap: CFRunLoop::current() returned a run loop; adding source to common modes"
            );
            let mode = common_modes().expect("kCFRunLoopCommonModes should be available on macOS");
            rl.add_source(Some(&source), Some(mode));
        } else {
            debug!("EventTap: CFRunLoop::current() returned None; run loop not present");
        }
        CGEvent::tap_enable(&port, true);

        let invalidation =
            InvalidationWatch::new(CFRetained::as_ptr(&port).as_ptr() as usize, on_invalidated);
        // SAFETY: `port_invalidated` reads neither the port nor its info, only the port's address.
        unsafe { port.set_invalidation_call_back(Some(port_invalidated)) };

        Ok(Self {
            _invalidation: invalidation,
            port,
            source,
            trampoline,
        })
    }

    /// Re-arms a tap the OS disabled. `false` means the port is dead and the tap
    /// has to be recreated.
    pub fn re_enable(&self) -> bool {
        CGEvent::tap_enable(&self.port, true);
        CGEvent::tap_is_enabled(&self.port)
    }
}

impl Drop for EventTap {
    fn drop(&mut self) {
        if self.port.is_valid() {
            CGEvent::tap_enable(&self.port, false);
        }
        if let Some(rl) = CFRunLoop::current() {
            rl.remove_source(Some(&self.source), common_modes());
        }
        // SAFETY: the tap is disabled and its source is off this thread's run loop, the one it was
        // added to as `EventTap` is not `Send`, so no callback reaches the trampoline again.
        unsafe { trampoline_drop(self.trampoline) };
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;

    #[test]
    fn a_failed_tap_creation_frees_what_it_was_handed_and_says_why() {
        let held = std::rc::Rc::new(());
        let result = hand_to_port(Box::new(std::rc::Rc::clone(&held)), |_| {
            Err::<(), _>(TapError::Create)
        });
        assert_eq!(result.err(), Some(TapError::Create));
        assert_eq!(std::rc::Rc::strong_count(&held), 1);
    }

    #[test]
    fn a_created_tap_keeps_what_it_was_handed() {
        let held = std::rc::Rc::new(());
        let mut seen = core::ptr::null_mut();
        let (_, ptr) = hand_to_port(Box::new(std::rc::Rc::clone(&held)), |ptr| {
            seen = ptr;
            Ok(())
        })
        .unwrap();
        assert_eq!(ptr, seen);
        assert_eq!(std::rc::Rc::strong_count(&held), 2);
        // SAFETY: `ptr` is the box `hand_to_port` kept on `Ok`, and nothing else frees it.
        drop(unsafe { Box::from_raw(ptr as *mut std::rc::Rc<()>) });
        assert_eq!(std::rc::Rc::strong_count(&held), 1);
    }

    /// A dead port calls the handler watched for it, never reading the info Core Foundation passes,
    /// which for a tap's port is Core Graphics' record rather than anything rini made.
    #[test]
    fn a_dead_port_calls_its_own_handler_and_never_reads_the_info() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let ports = [0u8; 2];
        let [port, other] = [&ports[0], &ports[1]].map(|p| p as *const u8 as *mut CFMachPort);
        let calls = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&calls);
        let watch = InvalidationWatch::new(
            port as usize,
            Arc::new(move || _ = counted.fetch_add(1, Ordering::SeqCst)),
        );
        let core_graphics_record = std::ptr::dangling_mut::<c_void>();

        port_invalidated(port, core_graphics_record);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        port_invalidated(other, core_graphics_record);
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "another port's death is not this one's"
        );
        drop(watch);
        port_invalidated(port, core_graphics_record);
        assert_eq!(calls.load(Ordering::SeqCst), 1, "a dropped tap is not told");
    }

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
        assert_eq!(
            governor.on_disabled(start + Duration::from_secs(2)),
            ReEnableDecision::Now
        );
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
        assert_eq!(
            decide(Recovery::TapDisabled(7), 7, &mut governor),
            Recovered::ReArm(7)
        );
    }

    /// The rule that kept a replaced tap from coming back. A message about an old generation must
    /// not re-arm anything: that tap is gone, and re-enabling it puts a second tap in the delivery
    /// path swallowing events.
    #[test]
    fn a_disable_of_a_replaced_tap_is_ignored() {
        let mut governor = ReEnableGovernor::new();
        assert_eq!(
            decide(Recovery::TapDisabled(6), 7, &mut governor),
            Recovered::Ignore
        );
        assert_eq!(
            decide(Recovery::CooldownElapsed(6), 7, &mut governor),
            Recovered::Ignore
        );
    }

    #[test]
    fn a_stale_disable_does_not_count_against_the_burst_limit() {
        let mut governor = ReEnableGovernor::new();
        for _ in 0..5 {
            assert_eq!(
                decide(Recovery::TapDisabled(1), 9, &mut governor),
                Recovered::Ignore
            );
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
        assert_eq!(
            on_recovery(Recovery::TapDisabled(1), 1, &mut governor, now),
            Recovered::ReArm(1)
        );
        assert_eq!(
            on_recovery(
                Recovery::TapDisabled(1),
                1,
                &mut governor,
                now + Duration::from_secs(1)
            ),
            Recovered::ReArm(1)
        );
        let third = on_recovery(
            Recovery::TapDisabled(1),
            1,
            &mut governor,
            now + Duration::from_secs(2),
        );
        match third {
            Recovered::StandDown { generation, wait } => {
                assert_eq!(generation, 1);
                assert!(
                    wait >= Duration::from_secs(1),
                    "a stand-down has to actually wait"
                );
            }
            other => panic!("expected a stand-down, got {other:?}"),
        }
    }

    #[test]
    fn a_cooldown_elapsing_on_the_live_tap_re_arms_it() {
        let mut governor = ReEnableGovernor::new();
        assert_eq!(
            decide(Recovery::CooldownElapsed(4), 4, &mut governor),
            Recovered::ReArm(4)
        );
    }

    /// An invalidated port is NOT generation-checked: it cannot be re-enabled at all, so whichever
    /// generation noticed, a new tap has to be built.
    #[test]
    fn an_invalidated_port_is_rebuilt_whatever_generation_reported_it() {
        let mut governor = ReEnableGovernor::new();
        assert_eq!(
            decide(Recovery::TapInvalidated(2), 9, &mut governor),
            Recovered::Rebuild(2)
        );
    }
}
