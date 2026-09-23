//! Input processing via a CGEventTap on a dedicated thread.
//!
//! The `EventTap` (aka input processor) owns a `Default`-mode CGEventTap and
//! runs its own CFRunLoop on a dedicated thread (`input` thread). This isolates
//! keyboard/mouse input processing from main-thread stalls (layout computation,
//! animation, WindowServer IPC).
//!
//! Shared state between the input thread and the main thread uses lock-free
//! `Arc<ArcSwap<T>>` primitives:
//! - `SharedHotkeyTable`: hotkey bindings, written by the input thread on
//!   config/layout changes, read by the callback.
//!
//! Requests from the main thread arrive via the actor channel (`Receiver`).
//! The main thread's `GestureTap` is a separate `ListenOnly` tap for gestures.

use std::cell::{Cell, RefCell};
use std::panic::AssertUnwindSafe;
use std::str::FromStr;
use std::sync::Arc;

use arc_swap::ArcSwap;
use objc2_core_foundation::CGPoint;
use objc2_core_graphics::{
    CGEvent, CGEventField, CGEventFlags, CGEventMask, CGEventSource, CGEventSourceStateID,
    CGEventTapOptions as CGTapOpt, CGEventTapProxy, CGEventType,
};
use tracing::{debug, error, trace, warn};

use crate::windows::platform::mouse::{MouseState, set_mouse_state};
use crate::windows::platform::window_server;
use rini_core::ids::WindowServerId;
use rini_runloop::channel;
use rustc_hash::FxHashMap as HashMap;

use crate::input::domain::binding::WmCommand;
use crate::input::domain::held_keys::HeldKeys;
use crate::input::domain::hotkey::modifiers_satisfy;
use crate::input::domain::key::{Hotkey, KeyCode};
use crate::input::domain::pointer;
use crate::input::event::{Event, EventSink};
use crate::input::platform::cursor;
use crate::input::platform::keyboard::{key_code_from_event, modifiers_from_flags_with_keys};
use crate::input::platform::tap;
use crate::input::settings::InputSettings;
const MOUSE_MOVE_MIN_INTERVAL_NS_NORMAL: u64 = 8_000_000; // 8ms ~= 125 Hz
const MOUSE_MOVE_MIN_INTERVAL_NS_LOW_POWER: u64 = 16_000_000; // 16ms ~= 62 Hz

#[derive(Debug)]
pub enum Request {
    Warp(CGPoint),
    HideOnFocus,
    EnforceHidden,
    SetEventProcessing(bool),
    SetFocusFollowsMouseEnabled(bool),
    SetHotkeys(Vec<(String, WmCommand)>),
    KeyboardLayoutChanged,
    SettingsUpdated(InputSettings),
    SetLowPowerMode(bool),
}

pub struct InputTap {
    events: Box<dyn EventSink>,
    requests_rx: Option<Receiver>,
    state: RefCell<State>,
    event_mask: Cell<CGEventMask>,
    mouse_move_last_timestamp: Cell<Option<u64>>,
    mouse_move_min_interval_ns: Cell<u64>,
    mouse_window: Cell<pointer::PointerCache>,
    tap: RefCell<Option<tap::EventTap>>,
    tap_generation: Cell<u64>,
    disable_hotkey: RefCell<Option<Hotkey>>,
    hotkey_specs: RefCell<Vec<(String, WmCommand)>>,
    hotkeys: SharedHotkeyTable,
}

// SAFETY: InputTap is constructed on the input thread and all access occurs on
// that same thread (CFRunLoop callback + channel recv both run on the input
// thread's run loop). The Send impl is required only to move the struct across
// the thread::spawn boundary.
unsafe impl Send for InputTap {}

struct State {
    hide_count: u32,
    mouse_hides_on_focus: bool,
    focus_follows_mouse_config_enabled: bool,
    event_processing_enabled: bool,
    focus_follows_mouse_enabled: bool,
    disable_hotkey_active: bool,
    low_power_mode: bool,
    held: HeldKeys,
    current_flags: CGEventFlags,
}

impl Default for State {
    fn default() -> Self {
        Self {
            hide_count: 0,
            mouse_hides_on_focus: false,
            focus_follows_mouse_config_enabled: false,
            event_processing_enabled: false,
            focus_follows_mouse_enabled: true,
            disable_hotkey_active: false,
            low_power_mode: false,
            held: HeldKeys::default(),
            current_flags: CGEventFlags::empty(),
        }
    }
}

pub type Sender = channel::Sender<Request>;
pub type Receiver = channel::Receiver<Request>;

pub type SharedHotkeyTable = Arc<ArcSwap<HashMap<Hotkey, Vec<WmCommand>>>>;

struct CallbackCtx {
    this: Arc<InputTap>,
    recovery_tx: tokio::sync::mpsc::UnboundedSender<tap::Recovery>,
    tap_generation: u64,
}

unsafe fn drop_mouse_ctx(ptr: *mut std::ffi::c_void) {
    unsafe { drop(Box::from_raw(ptr as *mut CallbackCtx)) };
}

impl InputTap {
    #[inline]
    fn keyboard_handlers_enabled(&self) -> bool {
        pointer::wants_keyboard_events(
            self.disable_hotkey.borrow().is_some(),
            self.hotkeys.load().len(),
        )
    }

    fn mouse_move_handlers_enabled(&self) -> bool {
        let state = self.state.borrow();
        pointer::wants_mouse_move_events(
            state.event_processing_enabled,
            state.focus_follows_mouse_config_enabled,
            state.focus_follows_mouse_enabled,
        )
    }

    fn desired_event_mask(&self) -> CGEventMask {
        build_event_mask(
            self.keyboard_handlers_enabled(),
            self.mouse_move_handlers_enabled(),
        )
    }

    fn create_tap_with_mask(
        self: &Arc<Self>,
        mask: CGEventMask,
        recovery_tx: tokio::sync::mpsc::UnboundedSender<tap::Recovery>,
    ) -> Option<tap::EventTap> {
        let tap_generation = self.tap_generation.get().wrapping_add(1);
        let ctx = Box::new(CallbackCtx {
            this: Arc::clone(self),
            recovery_tx,
            tap_generation,
        });
        let ctx_ptr = Box::into_raw(ctx) as *mut std::ffi::c_void;

        let tap = unsafe {
            tap::EventTap::new_with_options_and_recovery_callbacks(
                CGTapOpt::Default,
                mask,
                Some(mouse_callback),
                ctx_ptr,
                Some(drop_mouse_ctx),
                Some(event_tap_disabled),
                Some(event_tap_invalidated),
            )
        };

        if tap.is_none() {
            unsafe { drop(Box::from_raw(ctx_ptr as *mut CallbackCtx)) };
        }

        if tap.is_some() {
            self.tap_generation.set(tap_generation);
        }
        tap
    }

    fn rebuild_event_tap_mask_if_needed(
        self: &Arc<Self>,
        recovery_tx: &tokio::sync::mpsc::UnboundedSender<tap::Recovery>,
    ) {
        let next_mask = self.desired_event_mask();
        if next_mask == self.event_mask.get() {
            return;
        }

        let Some(new_tap) = self.create_tap_with_mask(next_mask, recovery_tx.clone()) else {
            warn!("Failed to rebuild event tap with updated mask");
            return;
        };

        let old_tap = self.tap.borrow_mut().replace(new_tap);
        drop(old_tap);
        self.event_mask.set(next_mask);
    }

    fn rebuild_invalidated_event_tap(
        self: &Arc<Self>,
        generation: u64,
        recovery_tx: &tokio::sync::mpsc::UnboundedSender<tap::Recovery>,
    ) {
        if generation != self.tap_generation.get() {
            debug!(generation, "Ignoring invalidation from a replaced event tap");
            return;
        }

        let mask = self.event_mask.get();
        let Some(new_tap) = self.create_tap_with_mask(mask, recovery_tx.clone()) else {
            error!(generation, "Failed to recreate invalidated event tap");
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
        let disable_hotkey = settings
            .focus_follows_mouse_disable_hotkey
            .clone()
            .and_then(|spec| spec.to_hotkey());
        let mut state = State::default();
        state.low_power_mode = low_power_mode;
        state.mouse_hides_on_focus = settings.mouse_hides_on_focus;
        state.focus_follows_mouse_config_enabled = settings.focus_follows_mouse;
        state.disable_hotkey_active = disable_hotkey
            .as_ref()
            .map(|target| state.compute_disable_hotkey_active(target))
            .unwrap_or(false);
        let event_mask = build_event_mask(
            disable_hotkey.is_some(),
            pointer::wants_mouse_move_events(
                state.event_processing_enabled,
                state.focus_follows_mouse_config_enabled,
                state.focus_follows_mouse_enabled,
            ),
        );
        let mouse_move_min_interval_ns = mouse_move_sampling_profile(state.low_power_mode);
        InputTap {
            events,
            requests_rx: Some(requests_rx),
            state: RefCell::new(state),
            event_mask: Cell::new(event_mask),
            mouse_move_last_timestamp: Cell::new(None),
            mouse_move_min_interval_ns: Cell::new(mouse_move_min_interval_ns),
            mouse_window: Cell::new(pointer::PointerCache::default()),
            tap: RefCell::new(None),
            tap_generation: Cell::new(0),
            disable_hotkey: RefCell::new(disable_hotkey),
            hotkey_specs: RefCell::new(Vec::new()),
            hotkeys: Arc::new(ArcSwap::from_pointee(HashMap::default())),
        }
    }

    pub async fn run(mut self) {
        let mut requests_rx = self.requests_rx.take().unwrap();
        let (recovery_tx, mut recovery_rx) = tokio::sync::mpsc::unbounded_channel();

        let this = Arc::new(self);

        let mask = this.event_mask.get();
        let tap = this.create_tap_with_mask(mask, recovery_tx.clone());

        if let Some(tap) = tap {
            *this.tap.borrow_mut() = Some(tap);
        } else {
            return;
        }

        if this.state.borrow().mouse_hides_on_focus {
            if let Err(e) = cursor::allow_hide_mouse() {
                error!(
                    "Could not enable mouse hiding: {e:?}. \
                    mouse_hides_on_focus will have no effect."
                );
            }
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
        self: &Arc<Self>,
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
        self: &Arc<Self>,
        request: Request,
        recovery_tx: &tokio::sync::mpsc::UnboundedSender<tap::Recovery>,
    ) {
        let mut should_rebuild_mask = false;
        let mut state = self.state.borrow_mut();
        match request {
            Request::Warp(point) => {
                self.reset_mouse_window();
                if let Err(e) = cursor::warp_mouse(point) {
                    warn!("Failed to warp mouse: {e:?}");
                }
                if state.mouse_hides_on_focus && state.hide_count == 0 {
                    debug!("Hiding mouse");
                    state.hide_mouse();
                }
            }
            Request::HideOnFocus => {
                if state.mouse_hides_on_focus && state.hide_count == 0 {
                    debug!("Hiding mouse after window focus changed");
                    state.hide_mouse();
                }
            }
            Request::EnforceHidden => {
                if state.hide_count > 0 {
                    state.hide_mouse();
                }
            }
            Request::SetEventProcessing(enabled) => {
                state.event_processing_enabled = enabled;
                if enabled {
                    self.reset_mouse_move_sample_gate();
                    self.reset_mouse_window();
                }
                should_rebuild_mask = true;
            }
            Request::SetFocusFollowsMouseEnabled(enabled) => {
                debug!(
                    "focus_follows_mouse temporarily {}",
                    if enabled { "enabled" } else { "disabled" }
                );
                state.focus_follows_mouse_enabled = enabled;
                if enabled {
                    self.reset_mouse_move_sample_gate();
                    self.reset_mouse_window();
                }
                should_rebuild_mask = true;
            }
            Request::SetHotkeys(bindings) => {
                *self.hotkey_specs.borrow_mut() = bindings;
                self.rebuild_hotkeys_for_current_layout();
                should_rebuild_mask = true;
            }
            Request::KeyboardLayoutChanged => {
                self.rebuild_hotkeys_for_current_layout();
                should_rebuild_mask = true;
            }
            Request::SettingsUpdated(settings) => {
                let mouse_hides_on_focus = settings.mouse_hides_on_focus;
                let focus_follows_mouse_config_enabled = settings.focus_follows_mouse;
                let disable_hotkey =
                    settings.focus_follows_mouse_disable_hotkey.and_then(|spec| spec.to_hotkey());
                *self.disable_hotkey.borrow_mut() = disable_hotkey;
                {
                    let prev_mouse_hides_on_focus = state.mouse_hides_on_focus;
                    let prev_focus_follows_mouse_config_enabled =
                        state.focus_follows_mouse_config_enabled;
                    state.mouse_hides_on_focus = mouse_hides_on_focus;
                    state.focus_follows_mouse_config_enabled = focus_follows_mouse_config_enabled;
                    let prev_active = state.disable_hotkey_active;
                    state.disable_hotkey_active = self
                        .disable_hotkey
                        .borrow()
                        .as_ref()
                        .map(|target| state.compute_disable_hotkey_active(target))
                        .unwrap_or(false);
                    if prev_active && !state.disable_hotkey_active {
                        self.reset_mouse_move_sample_gate();
                        self.reset_mouse_window();
                    }
                    if prev_focus_follows_mouse_config_enabled
                        != state.focus_follows_mouse_config_enabled
                    {
                        self.reset_mouse_move_sample_gate();
                        self.reset_mouse_window();
                    }
                    if prev_mouse_hides_on_focus
                        && !state.mouse_hides_on_focus
                        && state.hide_count > 0
                    {
                        debug!("Showing mouse after disabling mouse_hides_on_focus");
                        state.show_mouse();
                    }
                }
                should_rebuild_mask = true;
            }
            Request::SetLowPowerMode(enabled) => {
                if state.low_power_mode != enabled {
                    debug!("low_power_mode changed in event tap: {}", enabled);
                    state.low_power_mode = enabled;
                    self.mouse_move_min_interval_ns.set(mouse_move_sampling_profile(enabled));
                    self.reset_mouse_move_sample_gate();
                }
            }
        }
        drop(state);

        if should_rebuild_mask {
            self.rebuild_event_tap_mask_if_needed(recovery_tx);
        }
    }

    fn refresh_disable_hotkey_state(&self, state: &mut State) {
        let Some(target) = self.disable_hotkey.borrow().as_ref().cloned() else {
            return;
        };
        let prev_active = state.disable_hotkey_active;
        state.disable_hotkey_active = state.compute_disable_hotkey_active(&target);
        if state.disable_hotkey_active != prev_active {
            if state.disable_hotkey_active {
                debug!(?target, "focus_follows_mouse disabled while hotkey held");
            } else {
                debug!(?target, "focus_follows_mouse re-enabled after hotkey release");
                self.reset_mouse_move_sample_gate();
                self.reset_mouse_window();
            }
        }
    }

    #[inline]
    fn reset_mouse_move_sample_gate(&self) {
        self.mouse_move_last_timestamp.set(None);
    }

    #[inline]
    fn reset_mouse_window(&self) {
        self.mouse_window.set(pointer::PointerCache::default());
    }

    fn reconcile_after_tap_reenabled(&self) {
        let mut state = self.state.borrow_mut();
        let flags = CGEventSource::flags_state(CGEventSourceStateID::HIDSystemState);
        debug!(?flags, "Event tap was re-enabled; reconciling pressed keys");
        state.reconcile_after_event_tap_reenabled(flags);
        drop(state);
        self.refresh_disable_hotkey_state(&mut self.state.borrow_mut());
    }

    fn on_event(self: &Arc<Self>, event_type: CGEventType, event: &CGEvent) -> bool {
        if event_type == CGEventType::MouseMoved {
            return self.on_mouse_moved(event);
        }

        let mut state = self.state.borrow_mut();

        if !matches!(
            event_type,
            CGEventType::KeyDown | CGEventType::KeyUp | CGEventType::FlagsChanged
        ) {
            // Keep modifier-only hotkey state in sync even when macOS drops a
            // key-up/flags-changed event (common after system UI interruptions).
            let flags = CGEvent::flags(Some(event));
            if flags != state.current_flags {
                state.current_flags = flags;
                state.reconcile_modifier_keys();
                self.refresh_disable_hotkey_state(&mut state);
            }
        }

        match event_type {
            CGEventType::LeftMouseDown | CGEventType::RightMouseDown => {
                set_mouse_state(MouseState::Down);
            }
            CGEventType::LeftMouseDragged | CGEventType::RightMouseDragged => {
                set_mouse_state(MouseState::Down);
            }
            CGEventType::LeftMouseUp | CGEventType::RightMouseUp => set_mouse_state(MouseState::Up),
            _ => {}
        }

        if matches!(
            event_type,
            CGEventType::KeyDown | CGEventType::KeyUp | CGEventType::FlagsChanged
        ) {
            // App-directed shortcuts generated by rini must reach the application instead of
            // being interpreted as rini hotkeys again.
            if crate::windows::platform::mouse::is_rini_synthetic_event(event) {
                return true;
            }
            return self.handle_keyboard_event(event_type, event, &mut state);
        }

        if !state.event_processing_enabled {
            trace!("Mouse event processing disabled, ignoring {:?}", event_type);
            return true;
        }

        if state.hide_count > 0 {
            debug!("Showing mouse");
            state.show_mouse();
        }
        match event_type {
            CGEventType::RightMouseUp | CGEventType::LeftMouseUp => {
                self.events.send(Event::MouseUp);
            }
            _ => (),
        }

        true
    }

    /// Handle mouse moves without running the generic mouse/keyboard path.
    ///
    /// Mouse moves are usually the most frequent events delivered to this tap.
    /// In particular, do not read CGEvent flags for every hardware event: the
    /// keyboard and flags-changed events already maintain modifier state, and
    /// the sampled move path below is sufficient as a recovery check.
    fn on_mouse_moved(&self, event: &CGEvent) -> bool {
        let mut state = self.state.borrow_mut();
        if !state.event_processing_enabled {
            return true;
        }
        if state.hide_count > 0 {
            debug!("Showing mouse");
            state.show_mouse();
        }
        let loc = CGEvent::location(Some(event));

        // Recover modifier state at the sampled rate instead of once per raw
        // mouse event. Normal modifier transitions arrive through
        // FlagsChanged; this is only the defensive reconciliation path for
        // events lost while macOS UI interrupts the tap.
        if self.disable_hotkey.borrow().is_some() {
            let flags = CGEvent::flags(Some(event));
            if flags != state.current_flags {
                state.current_flags = flags;
                state.reconcile_modifier_keys();
                self.refresh_disable_hotkey_state(&mut state);
            }
        }

        // Resolve and deduplicate the window on the input thread. The application
        // only needs to see transitions; it must not receive a message for
        // every sampled point while the cursor remains in one window.
        if state.focus_follows_mouse_config_enabled
            && state.focus_follows_mouse_enabled
            && !state.disable_hotkey_active
        {
            let hint = mouse_window_hint(event);
            let previous = self.mouse_window.get();
            let window = Self::resolve_mouse_window(hint, loc, previous);
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
        hint: Option<WindowServerId>,
        point: CGPoint,
        previous: pointer::PointerCache,
    ) -> Option<WindowServerId> {
        match pointer::pointer_window(previous, hint) {
            pointer::PointerWindow::Cached(resolved) => resolved,
            pointer::PointerWindow::NeedsLookup => window_server::get_window_at_point(point),
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

    fn handle_keyboard_event(
        &self,
        event_type: CGEventType,
        event: &CGEvent,
        state: &mut State,
    ) -> bool {
        let key_code_opt = key_code_from_event(event);

        // FlagsChanged must be interpreted using the flags from this event,
        // rather than the previous event's modifier state.
        let flags = CGEvent::flags(Some(event));
        state.current_flags = flags;

        if let Some(key_code) = key_code_opt {
            match event_type {
                CGEventType::KeyDown => state.note_key_down(key_code),
                CGEventType::KeyUp => state.note_key_up(key_code),
                CGEventType::FlagsChanged => state.note_flags_changed(key_code),
                _ => {}
            }
        }
        self.refresh_disable_hotkey_state(state);

        if event_type == CGEventType::KeyDown {
            if let Some(key_code) = key_code_opt {
                let hotkey = Hotkey::new(
                    modifiers_from_flags_with_keys(state.current_flags, state.held.pressed()),
                    key_code,
                );
                let bindings = self.hotkeys.load();
                if let Some(commands) = bindings.get(&hotkey) {
                    // A held key generates repeated KeyDown events. Hotkeys
                    // are press-triggered, so dispatching those repeats can
                    // execute a command over and over. This is especially
                    // surprising for workspace_auto_back_and_forth, where
                    // each repeat toggles back to the other workspace.
                    let is_repeat = CGEvent::integer_value_field(
                        Some(event),
                        CGEventField::KeyboardEventAutorepeat,
                    ) != 0;
                    if is_repeat {
                        return false;
                    }
                    for cmd in commands {
                        self.events.send(Event::Command(cmd.clone()));
                    }
                    return false;
                }
            }
        }

        true
    }

    fn rebuild_hotkeys_for_current_layout(&self) {
        let specs = self.hotkey_specs.borrow();
        let mut map: HashMap<Hotkey, Vec<WmCommand>> = HashMap::default();

        for (spec, command) in specs.iter() {
            let Ok(hotkey) = Hotkey::from_str(spec) else {
                warn!(%spec, "Skipping hotkey that no longer resolves for current keyboard layout");
                continue;
            };

            if hotkey.modifiers.has_generic_modifiers() {
                for expanded_mods in hotkey.modifiers.expand_to_specific() {
                    let expanded_hotkey = Hotkey::new(expanded_mods, hotkey.key_code);
                    let entry = map.entry(expanded_hotkey).or_default();
                    if !entry.contains(command) {
                        entry.push(command.clone());
                    }
                }
            } else {
                let entry = map.entry(hotkey).or_default();
                if !entry.contains(command) {
                    entry.push(command.clone());
                }
            }
        }

        trace!(
            "Updated hotkey bindings for current keyboard layout: {}",
            map.len()
        );
        self.hotkeys.store(Arc::new(map));
    }
}

unsafe extern "C-unwind" fn mouse_callback(
    _proxy: CGEventTapProxy,
    event_type: CGEventType,
    event_ref: core::ptr::NonNull<CGEvent>,
    user_info: *mut std::ffi::c_void,
) -> *mut CGEvent {
    if user_info.is_null() {
        return event_ref.as_ptr();
    }
    let ctx = unsafe { &*(user_info as *const CallbackCtx) };
    let event = unsafe { event_ref.as_ref() };

    // Keep rejected high-frequency mouse events out of catch_unwind and the
    // actor/state path entirely. The admission check is scalar-only and has
    // no fallible or panicking operations.
    if event_type == CGEventType::MouseMoved && !ctx.this.admit_mouse_move(event) {
        return event_ref.as_ptr();
    }

    let result =
        std::panic::catch_unwind(AssertUnwindSafe(|| ctx.this.on_event(event_type, event)));

    match result {
        Ok(true) => event_ref.as_ptr(),
        Ok(false) => core::ptr::null_mut(),
        Err(_) => event_ref.as_ptr(),
    }
}

unsafe extern "C-unwind" fn event_tap_disabled(user_info: *mut std::ffi::c_void) {
    if user_info.is_null() {
        return;
    }
    let ctx = unsafe { &*(user_info as *const CallbackCtx) };
    let _ = ctx.recovery_tx.send(tap::Recovery::TapDisabled(ctx.tap_generation));
}

unsafe extern "C-unwind" fn event_tap_invalidated(user_info: *mut std::ffi::c_void) {
    if user_info.is_null() {
        return;
    }
    let ctx = unsafe { &*(user_info as *const CallbackCtx) };
    let _ = ctx.recovery_tx.send(tap::Recovery::TapInvalidated(ctx.tap_generation));
}

impl State {
    fn hide_mouse(&mut self) {
        if let Err(e) = cursor::hide_mouse() {
            warn!("Failed to hide mouse: {e:?}");
        }
        self.hide_count += 1;
    }

    fn show_mouse(&mut self) {
        while self.hide_count > 0 {
            if let Err(e) = cursor::show_mouse() {
                warn!("Failed to show mouse: {e:?}");
            }
            self.hide_count -= 1;
        }
    }

    fn note_key_down(&mut self, key_code: KeyCode) {
        self.held.key_down(key_code);
    }

    fn note_key_up(&mut self, key_code: KeyCode) {
        self.held.key_up(key_code);
    }

    fn note_flags_changed(&mut self, key_code: KeyCode) {
        self.held.flags_changed(self.current_flags.bits(), key_code);
    }

    fn reconcile_modifier_keys(&mut self) {
        self.held.observe_flags(self.current_flags.bits());
        self.held.reconcile_modifiers();
    }

    fn reconcile_after_event_tap_reenabled(&mut self, flags: CGEventFlags) {
        self.current_flags = flags;
        self.held.tap_re_enabled(flags.bits());
    }

    fn compute_disable_hotkey_active(&self, target: &Hotkey) -> bool {
        let active = modifiers_from_flags_with_keys(self.current_flags, self.held.pressed());
        modifiers_satisfy(target.modifiers, active) && self.held.is_held(target.key_code)
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

#[inline]
fn mouse_window_hint(event: &CGEvent) -> Option<WindowServerId> {
    let field_value =
        CGEvent::integer_value_field(Some(event), CGEventField::MouseEventWindowUnderMousePointer);
    u32::try_from(field_value).ok().filter(|id| *id != 0).map(WindowServerId::new)
}

fn build_event_mask(keyboard_enabled: bool, mouse_move_enabled: bool) -> CGEventMask {
    let mut m: u64 = 0;
    let add = |m: &mut u64, ty: CGEventType| *m |= 1u64 << (ty.0 as u64);

    for ty in [
        CGEventType::LeftMouseDown,
        CGEventType::LeftMouseUp,
        CGEventType::RightMouseDown,
        CGEventType::RightMouseUp,
        CGEventType::LeftMouseDragged,
        CGEventType::RightMouseDragged,
    ] {
        add(&mut m, ty);
    }
    if mouse_move_enabled {
        add(&mut m, CGEventType::MouseMoved);
    }
    if keyboard_enabled {
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

    fn wants(mask: CGEventMask, ty: CGEventType) -> bool {
        mask & (1u64 << (ty.0 as u64)) != 0
    }

    /// Mouse buttons are always asked for: they are how a drag is noticed, and a drag is how a
    /// window is moved between displays.
    #[test]
    fn the_mask_always_carries_the_mouse_buttons() {
        for enabled in [false, true] {
            let mask = build_event_mask(enabled, enabled);
            for ty in [
                CGEventType::LeftMouseDown,
                CGEventType::LeftMouseUp,
                CGEventType::RightMouseDown,
                CGEventType::RightMouseUp,
                CGEventType::LeftMouseDragged,
                CGEventType::RightMouseDragged,
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
        let without = build_event_mask(false, false);
        for ty in [
            CGEventType::KeyDown,
            CGEventType::KeyUp,
            CGEventType::FlagsChanged,
        ] {
            assert!(!wants(without, ty), "{ty:?} should not be asked for");
        }
        let with = build_event_mask(true, false);
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
        assert!(!wants(build_event_mask(false, false), CGEventType::MouseMoved));
        assert!(wants(build_event_mask(false, true), CGEventType::MouseMoved));
    }

    /// The two halves are independent: keyboard bindings must not drag mouse moves in with them, or
    /// every pointer movement becomes a window-server query on a machine that only uses hotkeys.
    #[test]
    fn the_two_halves_of_the_mask_are_independent() {
        assert!(!wants(build_event_mask(true, false), CGEventType::MouseMoved));
        assert!(!wants(build_event_mask(false, true), CGEventType::KeyDown));
    }

    #[test]
    fn tap_recovery_discards_cached_keys_and_uses_live_flags() {
        let mut state = State::default();
        state.held.key_down(KeyCode::ShiftLeft);
        state.held.key_down(KeyCode::KeyA);

        let live_flags = CGEventFlags::MaskShift | CGEventFlags::MaskCommand;
        state.reconcile_after_event_tap_reenabled(live_flags);

        assert!(state.held.pressed().is_empty());
        assert_eq!(state.current_flags, live_flags);
    }
}
