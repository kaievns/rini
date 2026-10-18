//! The session tap, on a thread of its own.
//!
//! `InputTap` owns a `Default`-mode CGEventTap and runs its own CFRunLoop on the `input` thread, so
//! a main-thread stall (layout, animation, window-server IPC) never holds an event in the tap.
//! Everything it holds stays on that thread: the tap's callback and the main thread's requests,
//! which arrive on the actor channel (`Receiver`), take turns on its run loop. The callback reads
//! each key event into a `Keystroke` and does what `KeyRouter` decides.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Instant;

use objc2_core_foundation::CGPoint;
use objc2_core_graphics::{
    CGEvent, CGEventField, CGEventMask, CGEventSource, CGEventSourceStateID,
    CGEventTapOptions as CGTapOpt, CGEventTapProxy, CGEventType,
};
use rini_core::ids::WindowServerId;
use rini_runloop::channel;
use tracing::{debug, error, trace, warn};

use crate::input::domain::binding::Bind;
use crate::input::domain::key::Hotkey;
use crate::input::domain::pointer;
use crate::input::domain::router::{Button, ButtonPhase, Click, KeyRouter, Keystroke};
use crate::input::domain::switch_session::{KeyEventKind, SessionId, Signal, SwitchKeys};
use crate::input::event::{Event, EventSink, LeftPress, OnLeftPress};
use crate::input::platform::keyboard::key_code_from_event;
use crate::input::platform::{cursor, tap};
use crate::input::settings::InputSettings;
use crate::windows::platform::mouse::{MouseState, is_rini_synthetic_event, set_mouse_state};
use crate::windows::platform::window_server::{self, WindowServer};

const MOUSE_MOVE_MIN_INTERVAL_NS_NORMAL: u64 = 8_000_000; // 8ms ~= 125 Hz
const MOUSE_MOVE_MIN_INTERVAL_NS_LOW_POWER: u64 = 16_000_000; // 16ms ~= 62 Hz

#[derive(Debug)]
pub enum Request {
    Warp(CGPoint),
    SetEventProcessing(bool),
    SetFocusFollowsMouseEnabled(bool),
    /// Every bind, its key free of `Mod`.
    SetHotkeys(Vec<Bind>),
    KeyboardLayoutChanged,
    SettingsUpdated(InputSettings),
    SetLowPowerMode(bool),
    /// The keys each switcher's session answers to; empty disables them all.
    SetSwitchKeys(Vec<SwitchKeys>),
    /// The session was ended from outside the tap — a switch committed by a click on the popup, an
    /// overview the reactor refused or closed — so stop treating it as open, unless a later one
    /// has opened since.
    EndSwitchSession(SessionId),
}

pub struct InputTap {
    events: Box<dyn EventSink>,
    on_left_press: Option<OnLeftPress>,
    timebase: pointer::Timebase,
    requests_rx: Option<Receiver>,
    mouse: Cell<MouseSettings>,
    /// The callback and the request loop share the tap through an `Rc`, so the router sits in a
    /// cell, borrowed for one call at a time.
    router: RefCell<KeyRouter>,
    event_mask: Cell<CGEventMask>,
    mouse_move_last_timestamp: Cell<Option<u64>>,
    mouse_move_min_interval_ns: Cell<u64>,
    mouse_window: Cell<pointer::PointerCache>,
    /// Where a pointer's window is looked up when the event names none.
    window_server: Arc<dyn WindowServer>,
    tap: RefCell<Option<tap::EventTap>>,
    tap_generation: Cell<u64>,
}

// SAFETY: `tap` is the one field that is not `Send`, and it is `None` until `run` makes a tap on the
// thread it runs on. `run` takes `self` into an `Rc` there, so no tap ever leaves that thread.
unsafe impl Send for InputTap {}

#[derive(Debug, Clone, Copy)]
struct MouseSettings {
    switches: pointer::MouseMoveSwitches,
    low_power_mode: bool,
}

pub type Sender = channel::Sender<Request>;
pub type Receiver = channel::Receiver<Request>;

struct CallbackCtx {
    this: Rc<InputTap>,
}

/// # Safety
///
/// `ptr` is a `CallbackCtx` `create_tap_with_mask` boxed, not freed before and not used after.
unsafe fn drop_mouse_ctx(ptr: *mut std::ffi::c_void) {
    // SAFETY: the caller hands over the box's only owner.
    unsafe { drop(Box::from_raw(ptr as *mut CallbackCtx)) };
}

impl InputTap {
    #[inline]
    fn keyboard_handlers_enabled(&self) -> bool {
        pointer::wants_keyboard_events(self.router.borrow().hotkey_count())
    }

    fn mouse_move_handlers_enabled(&self) -> bool {
        pointer::wants_mouse_move_events(self.mouse.get().switches)
    }

    fn desired_event_mask(&self) -> CGEventMask {
        build_event_mask(Asks {
            keys: self.keyboard_handlers_enabled(),
            mouse_moves: self.mouse_move_handlers_enabled(),
        })
    }

    fn create_tap_with_mask(
        self: &Rc<Self>,
        mask: CGEventMask,
        recovery_tx: tokio::sync::mpsc::UnboundedSender<tap::Recovery>,
    ) -> Option<tap::EventTap> {
        let generation = self.tap_generation.get().wrapping_add(1);
        let ctx = Box::new(CallbackCtx { this: Rc::clone(self) });
        let ctx_ptr = Box::into_raw(ctx) as *mut std::ffi::c_void;
        let disabled_tx = recovery_tx.clone();
        let on_disabled: tap::OnDisabled = Box::new(move || {
            let _ = disabled_tx.send(tap::Recovery::TapDisabled(generation));
        });
        let on_invalidated: tap::OnInvalidated = Arc::new(move || {
            let _ = recovery_tx.send(tap::Recovery::TapInvalidated(generation));
        });

        // SAFETY: `ctx_ptr` is the `CallbackCtx` `mouse_callback` reads and `drop_mouse_ctx` frees,
        // owned by the tap from here until it drops it.
        let tap = unsafe {
            tap::EventTap::new_with_options_and_recovery_callbacks(
                CGTapOpt::Default,
                mask,
                Some(mouse_callback),
                ctx_ptr,
                Some(drop_mouse_ctx),
                on_disabled,
                on_invalidated,
            )
        };

        match tap {
            Ok(tap) => {
                self.tap_generation.set(generation);
                Some(tap)
            }
            Err(error) => {
                // SAFETY: on `Err` the tap never took `ctx_ptr`, so this is its only owner.
                unsafe { drop_mouse_ctx(ctx_ptr) };
                error!(
                    mask = %format_args!("{mask:#x}"),
                    generation,
                    %error,
                    "Could not create the event tap"
                );
                None
            }
        }
    }

    fn rebuild_event_tap_mask_if_needed(
        self: &Rc<Self>,
        recovery_tx: &tokio::sync::mpsc::UnboundedSender<tap::Recovery>,
    ) {
        let next_mask = self.desired_event_mask();
        if next_mask == self.event_mask.get() {
            return;
        }

        let Some(new_tap) = self.create_tap_with_mask(next_mask, recovery_tx.clone()) else {
            return;
        };

        let old_tap = self.tap.borrow_mut().replace(new_tap);
        drop(old_tap);
        self.event_mask.set(next_mask);
        let signal = self.router.borrow_mut().tap_replaced();
        self.abandon_switch(signal);
    }

    fn rebuild_invalidated_event_tap(
        self: &Rc<Self>,
        generation: u64,
        recovery_tx: &tokio::sync::mpsc::UnboundedSender<tap::Recovery>,
    ) {
        if generation != self.tap_generation.get() {
            debug!(generation, "Ignoring invalidation from a replaced event tap");
            return;
        }

        let mask = self.event_mask.get();
        let Some(new_tap) = self.create_tap_with_mask(mask, recovery_tx.clone()) else {
            return;
        };

        let old_tap = self.tap.borrow_mut().replace(new_tap);
        drop(old_tap);
        self.reconcile_after_tap_reenabled();
        warn!(generation, "Recreated invalidated event tap");
    }

    /// `low_power_mode` is the machine's state at start; later changes arrive as
    /// [`Request::SetLowPowerMode`].
    pub fn new(
        settings: &InputSettings,
        low_power_mode: bool,
        events: Box<dyn EventSink>,
        requests_rx: Receiver,
    ) -> Self {
        let mouse = MouseSettings {
            switches: pointer::MouseMoveSwitches {
                processing: false,
                configured: settings.focus_follows_mouse,
                live: true,
            },
            low_power_mode,
        };
        let event_mask = build_event_mask(Asks {
            keys: pointer::wants_keyboard_events(0),
            mouse_moves: pointer::wants_mouse_move_events(mouse.switches),
        });
        let mouse_move_min_interval_ns = mouse_move_sampling_profile(mouse.low_power_mode);
        InputTap {
            events,
            on_left_press: None,
            timebase: mach_timebase(),
            requests_rx: Some(requests_rx),
            mouse: Cell::new(mouse),
            router: RefCell::new(KeyRouter::default()),
            event_mask: Cell::new(event_mask),
            mouse_move_last_timestamp: Cell::new(None),
            mouse_move_min_interval_ns: Cell::new(mouse_move_min_interval_ns),
            mouse_window: Cell::new(pointer::PointerCache::default()),
            window_server: window_server::live(),
            tap: RefCell::new(None),
            tap_generation: Cell::new(0),
        }
    }

    /// Where every left press goes, instead of through the sink.
    pub fn set_on_left_press(&mut self, on_left_press: OnLeftPress) {
        self.on_left_press = Some(on_left_press);
    }

    pub async fn run(mut self) {
        let mut requests_rx = self.requests_rx.take().unwrap();
        let (recovery_tx, mut recovery_rx) = tokio::sync::mpsc::unbounded_channel();

        let this = Rc::new(self);

        let mask = this.event_mask.get();
        let tap = this.create_tap_with_mask(mask, recovery_tx.clone());

        if let Some(tap) = tap {
            *this.tap.borrow_mut() = Some(tap);
        } else {
            return;
        }

        // Local to the input thread on purpose: the cooldown timer is a CFRunLoop timer for THIS
        // thread's run loop, and the governor's whole point is that only a healthy input thread
        // gets to re-arm the tap.
        let mut governor = tap::ReEnableGovernor::new();
        let mut _cooldown: Option<rini_runloop::run_loop::RepeatingTimer> = None;

        loop {
            tokio::select! {
                maybe_recovery = recovery_rx.recv() => {
                    let Some(recovery) = maybe_recovery else { break };
                    match tap::on_recovery(
                        recovery,
                        this.tap_generation.get(),
                        &mut governor,
                        std::time::Instant::now(),
                    ) {
                        tap::Recovered::ReArm(generation) => {
                            _cooldown = None;
                            this.re_enable_tap(generation, &recovery_tx);
                        }
                        tap::Recovered::StandDown { generation, wait } => {
                            warn!(?wait, "Event tap is being disabled repeatedly; standing down so input keeps flowing without it");
                            let tx = recovery_tx.clone();
                            _cooldown = rini_runloop::run_loop::RepeatingTimer::every(wait, move || {
                                _ = tx.send(tap::Recovery::CooldownElapsed(generation));
                            });
                            if _cooldown.is_none() {
                                tracing::error!("Could not start the re-enable cooldown; re-arming immediately");
                                this.re_enable_tap(generation, &recovery_tx);
                            }
                        }
                        tap::Recovered::Rebuild(generation) => {
                            this.rebuild_invalidated_event_tap(generation, &recovery_tx);
                        }
                        tap::Recovered::Ignore => {}
                    }
                }
                maybe_request = requests_rx.recv() => {
                    let Some((span, request)) = maybe_request else { break };
                    let _guard = span.enter();
                    this.on_request(request, &recovery_tx);
                }
            }
        }
    }

    /// Re-arms a disabled tap, falling back to recreation when the port is dead.
    fn re_enable_tap(
        self: &Rc<Self>,
        generation: u64,
        recovery_tx: &tokio::sync::mpsc::UnboundedSender<tap::Recovery>,
    ) {
        let re_enabled = self.tap.borrow().as_ref().is_some_and(|tap| tap.re_enable());
        if re_enabled {
            warn!(generation, "Re-enabled the event tap");
            self.reconcile_after_tap_reenabled();
        } else {
            error!(generation, "Event tap did not re-enable; recreating it");
            self.rebuild_invalidated_event_tap(generation, recovery_tx);
        }
    }

    fn on_request(
        self: &Rc<Self>,
        request: Request,
        recovery_tx: &tokio::sync::mpsc::UnboundedSender<tap::Recovery>,
    ) {
        let mut should_rebuild_mask = false;
        let mut mouse = self.mouse.get();
        match request {
            Request::Warp(point) => {
                self.reset_mouse_window();
                if let Err(e) = cursor::warp_mouse(point) {
                    warn!("Failed to warp mouse: {e:?}");
                }
            }
            Request::SetEventProcessing(enabled) => {
                mouse.switches.processing = enabled;
                if enabled {
                    self.reset_mouse_move_sampling();
                    self.reset_mouse_window();
                }
                should_rebuild_mask = true;
            }
            Request::SetFocusFollowsMouseEnabled(enabled) => {
                debug!(
                    "focus_follows_mouse temporarily {}",
                    if enabled { "enabled" } else { "disabled" }
                );
                mouse.switches.live = enabled;
                if enabled {
                    self.reset_mouse_move_sampling();
                    self.reset_mouse_window();
                }
                should_rebuild_mask = true;
            }
            Request::SetHotkeys(binds) => {
                let unresolved = self.router.borrow_mut().set_binds(binds, resolve_hotkey);
                self.note_hotkeys(unresolved);
                should_rebuild_mask = true;
            }
            Request::SetSwitchKeys(keys) => {
                let signal = self.router.borrow_mut().set_switch_keys(keys);
                self.send_switch(signal);
            }
            Request::EndSwitchSession(session) => {
                if self.router.borrow_mut().close_switch(session) {
                    debug!("session ended from outside the tap");
                }
            }
            Request::KeyboardLayoutChanged => {
                let unresolved = self.router.borrow_mut().resolve_binds(resolve_hotkey);
                self.note_hotkeys(unresolved);
                should_rebuild_mask = true;
            }
            Request::SettingsUpdated(settings) => {
                if mouse.switches.configured != settings.focus_follows_mouse {
                    mouse.switches.configured = settings.focus_follows_mouse;
                    self.reset_mouse_move_sampling();
                    self.reset_mouse_window();
                }
                should_rebuild_mask = true;
            }
            Request::SetLowPowerMode(enabled) => {
                if mouse.low_power_mode != enabled {
                    debug!("low_power_mode changed in event tap: {}", enabled);
                    mouse.low_power_mode = enabled;
                    self.mouse_move_min_interval_ns.set(mouse_move_sampling_profile(enabled));
                    self.reset_mouse_move_sampling();
                }
            }
        }
        self.mouse.set(mouse);

        if should_rebuild_mask {
            self.rebuild_event_tap_mask_if_needed(recovery_tx);
        }
    }

    #[inline]
    fn reset_mouse_move_sampling(&self) { self.mouse_move_last_timestamp.set(None); }

    #[inline]
    fn reset_mouse_window(&self) { self.mouse_window.set(pointer::PointerCache::default()); }

    fn reconcile_after_tap_reenabled(&self) {
        let flags = CGEventSource::flags_state(CGEventSourceStateID::HIDSystemState);
        debug!(?flags, "Event tap was re-enabled; reconciling pressed keys");
        let signal = self.router.borrow_mut().tap_re_enabled(flags.bits());
        self.abandon_switch(signal);
    }

    fn abandon_switch(&self, signal: Option<Signal>) {
        if signal.is_some() {
            debug!("abandoning a switcher session: the tap was replaced");
        }
        self.send_switch(signal);
    }

    fn send_switch(&self, signal: Option<Signal>) {
        if let Some(signal) = signal {
            self.events.send(Event::Switch(signal));
        }
    }

    fn note_hotkeys(&self, unresolved: Vec<String>) {
        for spec in unresolved {
            warn!(%spec, "Skipping hotkey that no longer resolves for current keyboard layout");
        }
        trace!(
            "Updated hotkey bindings for current keyboard layout: {}",
            self.router.borrow().hotkey_count()
        );
    }

    fn on_event(&self, event_type: CGEventType, event: &CGEvent) -> bool {
        if event_type == CGEventType::MouseMoved {
            return self.on_mouse_moved(event);
        }
        if let Some(kind) = key_event_kind(event_type) {
            return self.handle_keyboard_event(kind, event);
        }
        let Some((button, phase)) = button_event(event_type) else {
            return true;
        };
        let verdict = self.router.borrow_mut().on_click(Click {
            button,
            phase,
            flags: CGEvent::flags(Some(event)).bits(),
        });
        self.send_switch(verdict.signal);
        if verdict.swallow {
            return false;
        }
        if button == Button::Other {
            return true;
        }

        match phase {
            ButtonPhase::Down | ButtonPhase::Dragged => set_mouse_state(MouseState::Down),
            ButtonPhase::Up => set_mouse_state(MouseState::Up),
        }

        if !self.mouse.get().switches.processing {
            trace!("Mouse event processing disabled, ignoring {:?}", event_type);
            return true;
        }

        match event_type {
            CGEventType::RightMouseUp | CGEventType::LeftMouseUp => {
                self.events.send(Event::MouseUp);
            }
            CGEventType::LeftMouseDown => self.send_left_press(event),
            _ => (),
        }

        true
    }

    fn send_left_press(&self, event: &CGEvent) {
        let Some(on_left_press) = &self.on_left_press else {
            return;
        };
        let now = Instant::now();
        let age = pointer::event_age(
            mach_absolute_time(),
            CGEvent::timestamp(Some(event)),
            self.timebase,
        );
        on_left_press(LeftPress {
            location: CGEvent::location(Some(event)),
            at: now.checked_sub(age).unwrap_or(now),
            double_click: pointer::is_double_click(CGEvent::integer_value_field(
                Some(event),
                CGEventField::MouseEventClickState,
            )),
        });
    }

    /// Handle mouse moves without running the generic mouse/keyboard path.
    ///
    /// Mouse moves are usually the most frequent events delivered to this tap.
    /// In particular, do not read CGEvent flags for every hardware event: the
    /// keyboard and flags-changed events already maintain modifier state.
    fn on_mouse_moved(&self, event: &CGEvent) -> bool {
        let mouse = self.mouse.get();
        if !mouse.switches.processing {
            return true;
        }
        let loc = CGEvent::location(Some(event));

        // Resolve and deduplicate the window on the input thread. The application
        // only needs to see transitions; it must not receive a message for
        // every sampled point while the cursor remains in one window.
        if mouse.switches.configured && mouse.switches.live {
            let hint = mouse_window_hint(event);
            let previous = self.mouse_window.get();
            let window = self.resolve_mouse_window(hint, loc, previous);
            if previous.valid && previous.resolved == window {
                // Keep the hint current even when WindowServer resolves both
                // samples to the same window. This preserves the fast path
                // after a transient overlay changes the CGEvent hint.
                self.mouse_window.set(pointer::PointerCache {
                    hint,
                    resolved: window,
                    valid: true,
                });
                return true;
            }
            self.mouse_window.set(pointer::PointerCache {
                hint,
                resolved: window,
                valid: true,
            });
            if let Some(window) = window {
                window_server::note_windowserver_activity(window.as_u32());
                self.events.send(Event::PointerEnteredWindow(window));
            }
        }

        true
    }

    #[inline]
    fn resolve_mouse_window(
        &self,
        hint: Option<WindowServerId>,
        point: CGPoint,
        previous: pointer::PointerCache,
    ) -> Option<WindowServerId> {
        match pointer::pointer_window(previous, hint) {
            pointer::PointerWindow::Cached(resolved) => resolved,
            pointer::PointerWindow::NeedsLookup => self.window_server.get_window_at_point(point),
        }
    }

    /// Admit a mouse move for full processing. This deliberately contains
    /// only scalar `Cell` operations so it can run before the callback's
    /// panic boundary; rejected hardware events return directly to Core
    /// Graphics without entering the expensive Rust callback path.
    #[inline]
    fn admit_mouse_move(&self, event: &CGEvent) -> bool {
        let timestamp = CGEvent::timestamp(Some(event));
        if !pointer::admits_move(
            self.mouse_move_last_timestamp.get(),
            timestamp,
            self.mouse_move_min_interval_ns.get(),
        ) {
            return false;
        }
        self.mouse_move_last_timestamp.set(Some(timestamp));
        true
    }

    fn handle_keyboard_event(&self, kind: KeyEventKind, event: &CGEvent) -> bool {
        let autorepeat =
            CGEvent::integer_value_field(Some(event), CGEventField::KeyboardEventAutorepeat);
        let verdict = self.router.borrow_mut().on_key(Keystroke {
            kind,
            key: key_code_from_event(event),
            flags: CGEvent::flags(Some(event)).bits(),
            repeat: autorepeat != 0,
            synthetic: is_rini_synthetic_event(event),
            at: Instant::now(),
        });
        self.send_switch(verdict.signal);
        if let Some(action) = verdict.action {
            self.events.send(Event::Action(action));
        }
        !verdict.swallow
    }
}

fn key_event_kind(event_type: CGEventType) -> Option<KeyEventKind> {
    match event_type {
        CGEventType::KeyDown => Some(KeyEventKind::Down),
        CGEventType::KeyUp => Some(KeyEventKind::Up),
        CGEventType::FlagsChanged => Some(KeyEventKind::FlagsChanged),
        _ => None,
    }
}

fn button_event(event_type: CGEventType) -> Option<(Button, ButtonPhase)> {
    let read = match event_type {
        CGEventType::LeftMouseDown => (Button::Left, ButtonPhase::Down),
        CGEventType::LeftMouseDragged => (Button::Left, ButtonPhase::Dragged),
        CGEventType::LeftMouseUp => (Button::Left, ButtonPhase::Up),
        CGEventType::RightMouseDown => (Button::Right, ButtonPhase::Down),
        CGEventType::RightMouseDragged => (Button::Right, ButtonPhase::Dragged),
        CGEventType::RightMouseUp => (Button::Right, ButtonPhase::Up),
        CGEventType::OtherMouseDown => (Button::Other, ButtonPhase::Down),
        CGEventType::OtherMouseDragged => (Button::Other, ButtonPhase::Dragged),
        CGEventType::OtherMouseUp => (Button::Other, ButtonPhase::Up),
        _ => return None,
    };
    Some(read)
}

fn resolve_hotkey(spec: &str) -> Option<Hotkey> { Hotkey::from_str(spec).ok() }

/// # Safety
///
/// `user_info` is null or the `CallbackCtx` the tap was made with, still alive, and `event_ref` is
/// the event Core Graphics passes, valid for the call.
unsafe extern "C-unwind" fn mouse_callback(
    _proxy: CGEventTapProxy,
    event_type: CGEventType,
    event_ref: core::ptr::NonNull<CGEvent>,
    user_info: *mut std::ffi::c_void,
) -> *mut CGEvent {
    if user_info.is_null() {
        return event_ref.as_ptr();
    }
    // SAFETY: a live `CallbackCtx`, as the caller guarantees.
    let ctx = unsafe { &*(user_info as *const CallbackCtx) };
    // SAFETY: the event Core Graphics passes, valid for the call.
    let event = unsafe { event_ref.as_ref() };

    if event_type == CGEventType::MouseMoved && !ctx.this.admit_mouse_move(event) {
        return event_ref.as_ptr();
    }

    if ctx.this.on_event(event_type, event) {
        event_ref.as_ptr()
    } else {
        core::ptr::null_mut()
    }
}

#[inline]
fn mouse_move_sampling_profile(low_power_mode: bool) -> u64 {
    if low_power_mode {
        MOUSE_MOVE_MIN_INTERVAL_NS_LOW_POWER
    } else {
        MOUSE_MOVE_MIN_INTERVAL_NS_NORMAL
    }
}

#[repr(C)]
struct MachTimebaseInfo {
    numer: u32,
    denom: u32,
}

unsafe extern "C" {
    /// Takes nothing and reads a clock.
    safe fn mach_absolute_time() -> u64;
    fn mach_timebase_info(info: *mut MachTimebaseInfo) -> i32;
}

fn mach_timebase() -> pointer::Timebase {
    let mut info = MachTimebaseInfo { numer: 0, denom: 0 };
    // SAFETY: `info` is a live, writable `mach_timebase_info_data_t`, laid out as C declares it.
    if unsafe { mach_timebase_info(&mut info) } != 0 {
        warn!("mach_timebase_info failed; left presses are timed when the tap sees them");
    }
    pointer::Timebase {
        numer: info.numer,
        denom: info.denom,
    }
}

#[inline]
fn mouse_window_hint(event: &CGEvent) -> Option<WindowServerId> {
    let field_value =
        CGEvent::integer_value_field(Some(event), CGEventField::MouseEventWindowUnderMousePointer);
    u32::try_from(field_value).ok().filter(|id| *id != 0).map(WindowServerId::new)
}

/// Which optional events the tap asks for, beyond the mouse buttons it always does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Asks {
    keys: bool,
    mouse_moves: bool,
}

fn build_event_mask(asks: Asks) -> CGEventMask {
    let mut m: u64 = 0;
    let add = |m: &mut u64, ty: CGEventType| *m |= 1u64 << (ty.0 as u64);

    for ty in [
        CGEventType::LeftMouseDown,
        CGEventType::LeftMouseUp,
        CGEventType::RightMouseDown,
        CGEventType::RightMouseUp,
        CGEventType::LeftMouseDragged,
        CGEventType::RightMouseDragged,
        CGEventType::OtherMouseDown,
        CGEventType::OtherMouseUp,
        CGEventType::OtherMouseDragged,
    ] {
        add(&mut m, ty);
    }
    if asks.mouse_moves {
        add(&mut m, CGEventType::MouseMoved);
    }
    if asks.keys {
        for ty in [
            CGEventType::KeyDown,
            CGEventType::KeyUp,
            CGEventType::FlagsChanged,
        ] {
            add(&mut m, ty);
        }
    }
    m
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wants(mask: CGEventMask, ty: CGEventType) -> bool { mask & (1u64 << (ty.0 as u64)) != 0 }

    const NOTHING: Asks = Asks {
        keys: false,
        mouse_moves: false,
    };
    const KEYS: Asks = Asks { keys: true, mouse_moves: false };
    const MOUSE_MOVES: Asks = Asks { keys: false, mouse_moves: true };

    /// Mouse buttons are always asked for: they are how a drag is noticed, and a drag is how a
    /// window is moved between displays. Every button, the middle one included, since inside the
    /// overview a press of any of them is swallowed before it reaches a hidden window.
    #[test]
    fn the_mask_always_carries_the_mouse_buttons() {
        for enabled in [false, true] {
            let mask = build_event_mask(Asks {
                keys: enabled,
                mouse_moves: enabled,
            });
            for ty in [
                CGEventType::LeftMouseDown,
                CGEventType::LeftMouseUp,
                CGEventType::RightMouseDown,
                CGEventType::RightMouseUp,
                CGEventType::LeftMouseDragged,
                CGEventType::RightMouseDragged,
                CGEventType::OtherMouseDown,
                CGEventType::OtherMouseUp,
                CGEventType::OtherMouseDragged,
            ] {
                assert!(wants(mask, ty), "{ty:?} missing with enabled={enabled}");
            }
        }
    }

    /// The rule that keeps rini out of the keyboard path. An active tap holds each matching event
    /// until the callback answers, so asking for keys nobody is listening for puts rini between the
    /// user and every keystroke for nothing.
    #[test]
    fn keys_are_only_in_the_mask_when_something_is_bound() {
        let without = build_event_mask(NOTHING);
        for ty in [
            CGEventType::KeyDown,
            CGEventType::KeyUp,
            CGEventType::FlagsChanged,
        ] {
            assert!(!wants(without, ty), "{ty:?} should not be asked for");
        }
        let with = build_event_mask(KEYS);
        for ty in [
            CGEventType::KeyDown,
            CGEventType::KeyUp,
            CGEventType::FlagsChanged,
        ] {
            assert!(wants(with, ty), "{ty:?} should be asked for");
        }
    }

    #[test]
    fn mouse_moves_are_only_in_the_mask_when_focus_follows_mouse_is_live() {
        assert!(!wants(build_event_mask(NOTHING), CGEventType::MouseMoved));
        assert!(wants(build_event_mask(MOUSE_MOVES), CGEventType::MouseMoved));
    }

    /// The two halves are independent: keyboard bindings must not drag mouse moves in with them, or
    /// every pointer movement becomes a window-server query on a machine that only uses hotkeys.
    #[test]
    fn the_two_halves_of_the_mask_are_independent() {
        assert!(!wants(build_event_mask(KEYS), CGEventType::MouseMoved));
        assert!(!wants(build_event_mask(MOUSE_MOVES), CGEventType::KeyDown));
    }

    /// specs/input.md: rini MUST NOT tap trackpad gestures. A two-finger scroll arrives as a scroll
    /// wheel event; the rest are the NSEvent gesture types and the Dock's swipe (30).
    #[test]
    fn no_mask_asks_for_a_trackpad_gesture() {
        let gestures = [CGEventType::ScrollWheel]
            .into_iter()
            .chain([18, 19, 20, 29, 30, 31, 32].map(CGEventType));
        for ty in gestures {
            for keyboard in [false, true] {
                for mouse_move in [false, true] {
                    let mask = build_event_mask(Asks {
                        keys: keyboard,
                        mouse_moves: mouse_move,
                    });
                    assert!(
                        !wants(mask, ty),
                        "{ty:?} asked for with keyboard={keyboard} mouse_move={mouse_move}"
                    );
                }
            }
        }
    }
}
