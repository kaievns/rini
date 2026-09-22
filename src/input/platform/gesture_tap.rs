//! Gesture handling via a dedicated CGEventTap.
//!
//! This actor runs on the main thread and handles trackpad swipe/scroll
//! gestures for workspace switching.

use std::cell::{Cell, RefCell};
use std::panic::AssertUnwindSafe;
use std::rc::Rc;

use objc2::exception;
use objc2_app_kit::{NSEvent, NSEventPhase, NSEventType, NSTouchPhase, NSTouchType};
use objc2_core_graphics::{
    CGEvent, CGEventField, CGEventMask, CGEventTapLocation as CGTapLoc,
    CGEventTapOptions as CGTapOpt, CGEventTapProxy, CGEventType,
};
use tracing::{trace, warn};

use rini_ipc::protocol::{Command, LayoutCommand as LC};
use rini_runloop::channel;

use crate::input::domain::binding::WmCommand;
use crate::input::domain::gesture::{
    ScrollStep, SwipePhase, SwipeToward, SwipeTrack, normalized_fraction, scroll_step,
    touch_centroid,
};
use crate::input::event::{Event, EventSink};
use crate::input::platform::haptics::{self, HapticPattern};
use crate::input::settings::InputSettings;
use crate::input::platform::tap;
const K_CGS_EVENT_TYPE_FIELD: CGEventField = CGEventField(55);
const K_CGS_EVENT_DOCK_CONTROL: i64 = 30;
const K_GESTURE_HID_TYPE_FIELD: CGEventField = CGEventField(110);
const K_GESTURE_SWIPE_MOTION_FIELD: CGEventField = CGEventField(123);
const K_IOHID_EVENT_TYPE_DOCK_SWIPE: i64 = 23;
const K_CG_GESTURE_MOTION_HORIZONTAL: i64 = 1;

#[derive(Debug)]
pub enum GestureRequest {
    SettingsUpdated(InputSettings),
}

pub type Sender = channel::Sender<GestureRequest>;
pub type Receiver = channel::Receiver<GestureRequest>;

pub struct GestureTap {
    settings: RefCell<InputSettings>,
    events: Box<dyn EventSink>,
    swipe: RefCell<Option<SwipeHandler>>,
    scroll: RefCell<Option<ScrollHandler>>,
    tap: RefCell<Option<tap::EventTap>>,
    tap_generation: Cell<u64>,
    requests_rx: Option<Receiver>,
}

#[derive(Debug, Clone)]
struct SwipeConfig {
    enabled: bool,
    consume_dock_swipe: bool,
    invert_horizontal: bool,
    vertical_tolerance: f64,
    skip_empty_workspaces: Option<bool>,
    fingers: usize,
    distance_pct: f64,
    haptics_enabled: bool,
    haptic_pattern: HapticPattern,
}

impl SwipeConfig {
    fn from_settings(settings: &InputSettings) -> Self {
        let g = &settings.gestures;
        let vt_norm = normalized_fraction(g.swipe_vertical_tolerance);
        SwipeConfig {
            enabled: g.enabled,
            consume_dock_swipe: g.consume_dock_swipe,
            invert_horizontal: g.invert_horizontal_swipe,
            vertical_tolerance: vt_norm,
            skip_empty_workspaces: if g.skip_empty { Some(true) } else { None },
            fingers: g.fingers.max(1),
            distance_pct: g.distance_pct.clamp(0.01, 1.0),
            haptics_enabled: g.haptics_enabled,
            haptic_pattern: g.haptic_pattern,
        }
    }
}

struct SwipeHandler {
    cfg: SwipeConfig,
    state: RefCell<SwipeTrack>,
}

#[derive(Debug, Clone)]
struct ScrollConfig {
    enabled: bool,
    consume_dock_swipe: bool,
    invert_horizontal: bool,
    vertical_tolerance: f64,
    fingers: usize,
    distance_pct: f64,
}

impl ScrollConfig {
    fn from_settings(settings: &InputSettings) -> Self {
        let g = &settings.strip_scroll;
        let vt_norm = normalized_fraction(g.vertical_tolerance);
        ScrollConfig {
            enabled: g.enabled,
            consume_dock_swipe: settings.gestures.consume_dock_swipe,
            invert_horizontal: g.invert_horizontal,
            vertical_tolerance: vt_norm,
            fingers: g.fingers.max(1),
            distance_pct: g.distance_pct.clamp(0.01, 1.0),
        }
    }
}

#[derive(Default, Debug)]
struct ScrollState {
    phase: SwipePhase,
    start_x: f64,
    start_y: f64,
    last_x: f64,
    last_y: f64,
    accum_dx: f64,
    consuming: bool,
}

impl ScrollState {
    fn reset(&mut self) {
        self.phase = SwipePhase::Idle;
        self.start_x = 0.0;
        self.start_y = 0.0;
        self.last_x = 0.0;
        self.last_y = 0.0;
        self.accum_dx = 0.0;
        self.consuming = false;
    }
}

struct ScrollHandler {
    cfg: ScrollConfig,
    state: RefCell<ScrollState>,
}

struct CallbackCtx {
    this: Rc<GestureTap>,
    consumes: bool,
    recovery_tx: tokio::sync::mpsc::UnboundedSender<tap::Recovery>,
    tap_generation: u64,
}

unsafe fn drop_gesture_ctx(ptr: *mut std::ffi::c_void) {
    unsafe { drop(Box::from_raw(ptr as *mut CallbackCtx)) };
}

impl GestureTap {
    pub fn new(settings: InputSettings, events: Box<dyn EventSink>, requests_rx: Receiver) -> Self {
        let (swipe, scroll) = Self::build_gesture_handlers(&settings);
        GestureTap {
            settings: RefCell::new(settings),
            events,
            swipe: RefCell::new(swipe),
            scroll: RefCell::new(scroll),
            tap: RefCell::new(None),
            tap_generation: Cell::new(0),
            requests_rx: Some(requests_rx),
        }
    }

    pub async fn run(mut self) {
        let mut requests_rx = self.requests_rx.take().unwrap();
        let (recovery_tx, mut recovery_rx) = tokio::sync::mpsc::unbounded_channel();

        let this = Rc::new(self);

        if this.gesture_handlers_enabled() {
            this.create_and_install_tap(&recovery_tx);
        }

        // Same re-arm policy as the input tap: only a healthy thread re-enables, and a burst of
        // disables stands the tap down so macOS keeps delivering events without it.
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
                            warn!(?wait, "Gesture tap is being disabled repeatedly; standing down so input keeps flowing without it");
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
                            this.rebuild_invalidated_tap(generation, &recovery_tx);
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
            warn!(generation, "Re-enabled the gesture tap");
            self.reset_gesture_state();
        } else {
            tracing::error!(generation, "Gesture tap did not re-enable; recreating it");
            self.rebuild_invalidated_tap(generation, recovery_tx);
        }
    }

    fn on_request(
        self: &Rc<Self>,
        request: GestureRequest,
        recovery_tx: &tokio::sync::mpsc::UnboundedSender<tap::Recovery>,
    ) {
        match request {
            GestureRequest::SettingsUpdated(settings) => {
                *self.settings.borrow_mut() = settings;
                self.update_gesture_handlers(recovery_tx);
            }
        }
    }

    fn build_gesture_handlers(settings: &InputSettings) -> (Option<SwipeHandler>, Option<ScrollHandler>) {
        let swipe_cfg = SwipeConfig::from_settings(settings);
        let swipe = if swipe_cfg.enabled {
            Some(SwipeHandler {
                cfg: swipe_cfg,
                state: RefCell::new(SwipeTrack::default()),
            })
        } else {
            None
        };

        let scroll_cfg = ScrollConfig::from_settings(settings);
        let scroll = if scroll_cfg.enabled {
            Some(ScrollHandler {
                cfg: scroll_cfg,
                state: RefCell::new(ScrollState::default()),
            })
        } else {
            None
        };

        (swipe, scroll)
    }

    fn update_gesture_handlers(
        self: &Rc<Self>,
        recovery_tx: &tokio::sync::mpsc::UnboundedSender<tap::Recovery>,
    ) {
        let settings = self.settings.borrow();
        let (swipe, scroll) = Self::build_gesture_handlers(&settings);
        let was_enabled = self.gesture_handlers_enabled();
        *self.swipe.borrow_mut() = swipe;
        *self.scroll.borrow_mut() = scroll;
        let is_enabled = self.gesture_handlers_enabled();

        if !was_enabled && is_enabled {
            self.create_and_install_tap(recovery_tx);
        } else if was_enabled && !is_enabled {
            *self.tap.borrow_mut() = None;
        }
    }

    fn gesture_handlers_enabled(&self) -> bool {
        self.swipe.borrow().is_some() || self.scroll.borrow().is_some()
    }

    fn create_and_install_tap(
        self: &Rc<Self>,
        recovery_tx: &tokio::sync::mpsc::UnboundedSender<tap::Recovery>,
    ) {
        let mask = gesture_event_mask();
        let tap_location = CGTapLoc::HIDEventTap;
        let tap_generation = self.tap_generation.get().wrapping_add(1);
        let tap = unsafe {
            let ctx_ptr = Box::into_raw(Box::new(CallbackCtx {
                this: Rc::clone(self),
                consumes: true,
                recovery_tx: recovery_tx.clone(),
                tap_generation,
            })) as *mut std::ffi::c_void;
            match tap::EventTap::new_at_location_with_options_and_recovery_callbacks(
                tap_location,
                CGTapOpt::Default,
                mask,
                Some(gesture_callback),
                ctx_ptr,
                Some(drop_gesture_ctx),
                Some(gesture_tap_disabled),
                Some(gesture_tap_invalidated),
            ) {
                Some(tap) => Some(tap),
                None => {
                    drop(Box::from_raw(ctx_ptr as *mut CallbackCtx));
                    let ctx_ptr = Box::into_raw(Box::new(CallbackCtx {
                        this: Rc::clone(self),
                        consumes: false,
                        recovery_tx: recovery_tx.clone(),
                        tap_generation,
                    })) as *mut std::ffi::c_void;
                    match tap::EventTap::new_at_location_with_options_and_recovery_callbacks(
                        tap_location,
                        CGTapOpt::ListenOnly,
                        mask,
                        Some(gesture_callback),
                        ctx_ptr,
                        Some(drop_gesture_ctx),
                        Some(gesture_tap_disabled),
                        Some(gesture_tap_invalidated),
                    ) {
                        Some(tap) => {
                            warn!(
                                "Falling back to listen-only HID gesture tap; workspace swipe events will pass through to macOS"
                            );
                            Some(tap)
                        }
                        None => {
                            drop(Box::from_raw(ctx_ptr as *mut CallbackCtx));
                            None
                        }
                    }
                }
            }
        };

        if let Some(tap) = tap {
            self.tap_generation.set(tap_generation);
            *self.tap.borrow_mut() = Some(tap);
        } else {
            tracing::warn!("Failed to create gesture event tap");
        }
    }

    fn rebuild_invalidated_tap(
        self: &Rc<Self>,
        generation: u64,
        recovery_tx: &tokio::sync::mpsc::UnboundedSender<tap::Recovery>,
    ) {
        if generation != self.tap_generation.get() || !self.gesture_handlers_enabled() {
            trace!(generation, "Ignoring invalidation from a replaced gesture tap");
            return;
        }

        self.reset_gesture_state();
        self.create_and_install_tap(recovery_tx);
        warn!(generation, "Recreated invalidated gesture event tap");
    }

    fn reset_gesture_state(&self) {
        if let Some(handler) = self.swipe.borrow().as_ref() {
            handler.state.borrow_mut().reset();
        }
        if let Some(handler) = self.scroll.borrow().as_ref() {
            handler.state.borrow_mut().reset();
        }
    }

    fn on_event(self: &Rc<Self>, event_type: CGEventType, event: &CGEvent) -> bool {
        let scroll_handler = self.scroll.borrow();
        let swipe_handler = self.swipe.borrow();
        if scroll_handler.is_none() && swipe_handler.is_none() {
            return true;
        }

        if is_physical_horizontal_dock_swipe(event_type, event) {
            let consume = scroll_handler.as_ref().is_some_and(|handler| handler.cfg.consume_dock_swipe);
            return !consume;
        }

        if event_type.0 != NSEventType::Gesture.0 as u32 {
            return true;
        }

        if let Some(nsevent) = NSEvent::eventWithCGEvent(event)
            && nsevent.r#type() == NSEventType::Gesture
        {
            if let Some(handler) = scroll_handler.as_ref() {
                return !self.handle_scroll_gesture_event(handler, &nsevent);
            } else if let Some(handler) = swipe_handler.as_ref() {
                return !self.handle_gesture_event(handler, &nsevent);
            }
        }

        true
    }

    /// Returns whether this event belongs to a horizontal swipe Rini owns and
    /// should therefore be suppressed at the event tap.
    /// Read one `NSEvent` into a centroid, hand it to `SwipeTrack`, and act on the answer.
    ///
    /// The phases and the travel arithmetic are both in `input::domain::gesture`; what is left here
    /// is the AppKit reading and the two side effects — the haptic and the command.
    fn handle_gesture_event(&self, handler: &SwipeHandler, nsevent: &NSEvent) -> bool {
        let cfg = &handler.cfg;
        let mut track = handler.state.borrow_mut();

        let phase = nsevent.phase();
        if matches!(phase, NSEventPhase::Ended | NSEventPhase::Cancelled) {
            return cfg.consume_dock_swipe && track.end();
        }
        if matches!(phase, NSEventPhase::Began) {
            track.reset();
        }

        let mut sum = (0.0f64, 0.0f64);
        let mut touch_count = 0usize;
        let mut active_count = 0usize;
        let mut too_many_touches = false;
        for touch in nsevent.allTouches().iter() {
            let phase = touch.phase();
            let ended =
                phase.contains(NSTouchPhase::Ended) || phase.contains(NSTouchPhase::Cancelled);
            touch_count += 1;
            if touch_count > cfg.fingers {
                too_many_touches = true;
                break;
            }
            if !ended && let Some((x, y)) = touch_normalized_position(&touch) {
                sum.0 += x;
                sum.1 += y;
                active_count += 1;
            }
        }

        let centroid = (!too_many_touches)
            .then(|| touch_centroid(touch_count, active_count, sum, cfg.fingers))
            .flatten();
        let Some(centroid) = centroid else {
            return cfg.consume_dock_swipe && track.end();
        };

        let outcome = track.advance(
            centroid,
            active_count,
            cfg.vertical_tolerance,
            cfg.distance_pct,
            cfg.invert_horizontal,
        );
        if let Some(toward) = outcome.commit {
            let cmd = match toward {
                SwipeToward::Next => LC::NextWorkspace(cfg.skip_empty_workspaces),
                SwipeToward::Prev => LC::PrevWorkspace(cfg.skip_empty_workspaces),
            };
            if cfg.haptics_enabled {
                let _ = haptics::perform_haptic(cfg.haptic_pattern);
            }
            self.events
                .send(Event::Command(WmCommand::ReactorCommand(Command::Layout(cmd))));
        }
        cfg.consume_dock_swipe && outcome.consume
    }

    /// One frame of a horizontal scroll gesture, whatever phase it is in.
    ///
    /// Returns the strip movement to send, if this frame carried the accumulator past the step. An
    /// off-axis frame leaves the accumulator and the consuming flag where they were, so a gesture that
    /// wanders for a frame resumes rather than restarting.
    fn advance_scroll(
        &self,
        cfg: &ScrollConfig,
        st: &mut ScrollState,
        centroid: (f64, f64),
    ) -> Option<f64> {
        let delta = (centroid.0 - st.last_x, centroid.1 - st.last_y);
        st.last_x = centroid.0;
        st.last_y = centroid.1;
        match scroll_step(delta, st.accum_dx, cfg.vertical_tolerance, cfg.distance_pct, cfg.invert_horizontal) {
            ScrollStep::OffAxis => None,
            ScrollStep::Accumulating { accumulated } => {
                st.consuming = true;
                st.accum_dx = accumulated;
                None
            }
            ScrollStep::Scroll { delta } => {
                st.consuming = true;
                st.accum_dx = 0.0;
                Some(delta)
            }
        }
    }

    fn send_scroll(&self, delta: f64) {
        let cmd = LC::ScrollStrip { delta };
        self.events.send(Event::Command(WmCommand::ReactorCommand(Command::Layout(cmd))));
    }

    /// Returns whether this event belongs to a horizontal scrolling gesture
    /// Rini owns and should therefore be suppressed at the event tap.
    fn handle_scroll_gesture_event(&self, handler: &ScrollHandler, nsevent: &NSEvent) -> bool {
        let cfg = &handler.cfg;
        let state = &handler.state;

        let mut st = state.borrow_mut();

        let phase = nsevent.phase();
        if matches!(phase, NSEventPhase::Ended | NSEventPhase::Cancelled) {
            let consuming = st.consuming;
            st.reset();
            return cfg.consume_dock_swipe && consuming;
        }
        if matches!(phase, NSEventPhase::Began) {
            st.reset();
        }

        let touches = nsevent.allTouches();
        let mut sum_x = 0.0f64;
        let mut sum_y = 0.0f64;
        let mut touch_count = 0usize;
        let mut active_count = 0usize;
        let mut too_many_touches = false;
        let mut all_moved = true;

        for t in touches.iter() {
            let phase = t.phase();
            if phase.contains(NSTouchPhase::Stationary) {
                all_moved = false;
                continue;
            }

            if !phase.contains(NSTouchPhase::Moved) {
                all_moved = false;
            }

            let ended =
                phase.contains(NSTouchPhase::Ended) || phase.contains(NSTouchPhase::Cancelled);

            touch_count += 1;
            if touch_count > cfg.fingers {
                too_many_touches = true;
                break;
            }

            if !ended && let Some((x, y)) = touch_normalized_position(&t) {
                sum_x += x;
                sum_y += y;
                active_count += 1;
            }
        }

        let centroid = (!too_many_touches)
            .then(|| touch_centroid(touch_count, active_count, (sum_x, sum_y), cfg.fingers))
            .flatten();
        let Some((avg_x, avg_y)) = centroid else {
            let consuming = st.consuming;
            st.reset();
            return cfg.consume_dock_swipe && consuming;
        };

        match st.phase {
            SwipePhase::Idle => {
                st.start_x = avg_x;
                st.start_y = avg_y;
                st.last_x = avg_x;
                st.last_y = avg_y;
                st.accum_dx = 0.0;
                st.phase = SwipePhase::Armed;
                trace!(
                    "scroll armed: start_x={:.3} start_y={:.3}",
                    st.start_x, st.start_y
                );
            }
            SwipePhase::Armed => {
                if !all_moved {
                    st.last_x = avg_x;
                    st.last_y = avg_y;
                    return cfg.consume_dock_swipe && st.consuming;
                }

                if let Some(delta) = self.advance_scroll(cfg, &mut st, (avg_x, avg_y)) {
                    self.send_scroll(delta);
                    st.phase = SwipePhase::Committed;
                }
            }
            SwipePhase::Committed => {
                if active_count == 0 {
                    let consuming = st.consuming;
                    st.reset();
                    return cfg.consume_dock_swipe && consuming;
                } else if all_moved
                    && let Some(delta) = self.advance_scroll(cfg, &mut st, (avg_x, avg_y))
                {
                    self.send_scroll(delta);
                }
            }
        }

        cfg.consume_dock_swipe && st.consuming
    }
}

fn gesture_event_mask() -> CGEventMask {
    (1u64 << (NSEventType::Gesture.0 as u64)) | (1u64 << (K_CGS_EVENT_DOCK_CONTROL as u64))
}

fn is_physical_horizontal_dock_swipe(event_type: CGEventType, event: &CGEvent) -> bool {
    let cgs_type = CGEvent::integer_value_field(Some(event), K_CGS_EVENT_TYPE_FIELD);
    let hid_type = CGEvent::integer_value_field(Some(event), K_GESTURE_HID_TYPE_FIELD);
    let motion = CGEvent::integer_value_field(Some(event), K_GESTURE_SWIPE_MOTION_FIELD);

    (event_type.0 as i64 == K_CGS_EVENT_DOCK_CONTROL || cgs_type == K_CGS_EVENT_DOCK_CONTROL)
        && hid_type == K_IOHID_EVENT_TYPE_DOCK_SWIPE
        && motion == K_CG_GESTURE_MOTION_HORIZONTAL
}

#[inline]
fn touch_normalized_position(touch: &objc2_app_kit::NSTouch) -> Option<(f64, f64)> {
    if touch.r#type() != NSTouchType::Indirect || touch.isResting() {
        return None;
    }

    let position = std::panic::catch_unwind(AssertUnwindSafe(|| {
        exception::catch(AssertUnwindSafe(|| touch.normalizedPosition())).ok()
    }))
    .ok()
    .flatten()?;
    let x = position.x.clamp(0.0, 1.0) as f64;
    let y = position.y.clamp(0.0, 1.0) as f64;
    Some((x, y))
}

unsafe extern "C-unwind" fn gesture_callback(
    _proxy: CGEventTapProxy,
    event_type: CGEventType,
    event_ref: core::ptr::NonNull<CGEvent>,
    user_info: *mut std::ffi::c_void,
) -> *mut CGEvent {
    let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
        let ctx = unsafe { &*(user_info as *const CallbackCtx) };
        let event = unsafe { event_ref.as_ref() };
        (ctx.this.on_event(event_type, event), ctx.consumes)
    }));

    match result {
        Ok((true, _)) => event_ref.as_ptr(),
        Ok((false, true)) => core::ptr::null_mut(),
        Ok((false, false)) => event_ref.as_ptr(),
        Err(_) => event_ref.as_ptr(),
    }
}

unsafe extern "C-unwind" fn gesture_tap_disabled(user_info: *mut std::ffi::c_void) {
    if user_info.is_null() {
        return;
    }
    let ctx = unsafe { &*(user_info as *const CallbackCtx) };
    let _ = ctx.recovery_tx.send(tap::Recovery::TapDisabled(ctx.tap_generation));
}

unsafe extern "C-unwind" fn gesture_tap_invalidated(user_info: *mut std::ffi::c_void) {
    if user_info.is_null() {
        return;
    }
    let ctx = unsafe { &*(user_info as *const CallbackCtx) };
    let _ = ctx.recovery_tx.send(tap::Recovery::TapInvalidated(ctx.tap_generation));
}
