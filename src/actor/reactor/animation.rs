use std::time::Duration;

use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use tokio::sync::mpsc;
use tracing::{debug, trace};

use super::TransactionId;
use crate::actor::app::{AppThreadHandle, Request, WindowId, pid_t};
use crate::actor::reactor::Reactor;
use crate::common::collections::HashMap;
use crate::common::config::Config;
use crate::sys::geometry::{Round, SameAs};
use crate::sys::power;
use crate::sys::screen::SpaceId;
use crate::sys::timer::Timer;
use crate::sys::window_server::WindowServerId;

pub type Sender = mpsc::UnboundedSender<Message>;
pub type Receiver = mpsc::UnboundedReceiver<Message>;

#[derive(Debug)]
pub enum Message {
    Replace(Animation),
    SkipToEnd(Animation),
}

#[derive(Debug, Default)]
pub struct AnimationManager {
    active: Option<ActiveAnimation>,
}

#[derive(Debug)]
struct ActiveAnimation {
    animation: Animation,
    next_frame: u32,
}

#[derive(Debug)]
pub struct Animation {
    interval: Duration,
    frames: u32,
    windows: Vec<AnimatedWindow>,
    handled_windows: Vec<WindowId>,
}

#[derive(Debug)]
struct AnimatedWindow {
    handle: AppThreadHandle,
    wid: WindowId,
    start: CGRect,
    finish: CGRect,
    /// Kept for callers and debugging, but no longer changes the animation: the
    /// focused window used to be special-cased into snapping straight to its final
    /// size, which is what made resizes pop instead of easing.
    #[allow(dead_code)]
    is_focus: bool,
    txid: TransactionId,
}

impl AnimatedWindow {
    fn frame_after(&self, frame: u32, total_frames: u32) -> CGRect {
        // Interpolate SIZE as well as position.
        //
        // This used to snap the size instead of easing it: the focused window got
        // `finish.size` at frame 0, and every other window held `start.size` until
        // halfway and then jumped. Position eased smoothly the whole time, so a
        // resize read as "the neighbour glides into place, then the focused window
        // pops to its new width" — with a visible gap in between while the
        // neighbour had moved but the focused window had not yet grown.
        //
        // get_frame already blends both origin and size on the same eased curve,
        // so the two now finish together.
        if frame == 0 {
            return self.start;
        }
        let t = f64::from(frame) / f64::from(total_frames);
        get_frame(self.start, self.finish, t)
    }
}

impl AnimationManager {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn run(mut rx: Receiver) {
        let mut manager = Self::new();
        let mut tick_timer = Timer::manual();

        loop {
            tokio::select! {
                message = rx.recv() => {
                    let Some(message) = message else {
                        manager.finish_active();
                        break;
                    };
                    if let Some(delay) = manager.handle_message(message) {
                        tick_timer.set_next_fire(delay);
                    }
                }
                _ = tick_timer.next(), if manager.active.is_some() => {
                    if let Some(delay) = manager.tick() {
                        tick_timer.set_next_fire(delay);
                    }
                }
            }
        }
    }

    pub fn handle_message(&mut self, message: Message) -> Option<Duration> {
        match message {
            Message::Replace(animation) => {
                self.active = match self.active.take() {
                    Some(active) => Some(active.replace_with(animation)),
                    None => ActiveAnimation::start(animation),
                };
                self.active.as_ref().map(|active| active.animation.interval)
            }
            Message::SkipToEnd(animation) => {
                self.finish_active();
                animation.skip_to_end();
                None
            }
        }
    }

    pub fn tick(&mut self) -> Option<Duration> {
        let active = self.active.as_mut()?;
        active.send_next_frame();
        if active.is_complete() {
            let active = self.active.take().expect("animation disappeared while ticking");
            active.animation.end();
            None
        } else {
            Some(active.animation.interval)
        }
    }

    fn finish_active(&mut self) {
        if let Some(active) = self.active.take() {
            active.animation.skip_to_end_and_end();
        }
    }

    pub fn animate_layout(
        reactor: &mut Reactor,
        space: SpaceId,
        layout: &[(WindowId, CGRect)],
        is_resize: bool,
        skip_wid: Option<WindowId>,
    ) -> bool {
        let Some(active_ws) = reactor.layout_manager.layout_engine.active_workspace(space) else {
            return false;
        };
        let mut anim = Animation::new(reactor.config.clone());
        let mut animated_count = 0;
        let mut any_frame_changed = false;

        for &(wid, target_frame) in layout {
            if skip_wid == Some(wid) {
                anim.mark_handled(wid);
                trace!(
                    ?wid,
                    "Skipping animated layout update for window currently being dragged"
                );
                continue;
            }

            let target_frame = target_frame.round();
            let (current_frame, window_server_id, txid) = {
                let window_store = &mut reactor.state.windows;
                match window_store.window_mut(wid) {
                    Some(window) => {
                        let current_frame = window.frame_monotonic;
                        if target_frame.same_as(current_frame) {
                            continue;
                        }
                        let wsid = window.info.sys_id;
                        if let Some(wsid) = wsid {
                            if reactor
                                .transaction_manager
                                .get_target_frame(wsid)
                                .is_some_and(|pending| pending.same_as(target_frame))
                            {
                                trace!(?wid, ?target_frame, "Skipping redundant layout request");
                                continue;
                            }
                        }
                        any_frame_changed = true;
                        let txid = wsid
                            .map(|wsid| reactor.transaction_manager.generate_next_txid(wsid))
                            .unwrap_or_default();
                        (current_frame, wsid, txid)
                    }
                    None => {
                        debug!(?wid, "Skipping - window no longer exists");
                        continue;
                    }
                }
            };

            let Some(app_state) = &reactor.app_manager.apps.get(&wid.pid) else {
                debug!(?wid, "Skipping for window - app no longer exists");
                continue;
            };

            let is_active = reactor
                .layout_manager
                .layout_engine
                .virtual_workspace_manager()
                .workspace_for_window(&reactor.state.windows, space, wid)
                .is_some_and(|ws| ws == active_ws);

            if is_active {
                trace!(?wid, ?current_frame, ?target_frame, "Animating visible window");
                anim.add_window(&app_state.handle, wid, current_frame, target_frame, false, txid);
                animated_count += 1;
                if let Some(wsid) = window_server_id {
                    reactor.transaction_manager.update_txid_entries([(wsid, txid, target_frame)]);
                }
            } else {
                anim.mark_handled(wid);
                trace!(
                    ?wid,
                    ?current_frame,
                    ?target_frame,
                    "Direct positioning hidden window"
                );
                if let Some(wsid) = window_server_id {
                    reactor.transaction_manager.update_txid_entries([(wsid, txid, target_frame)]);
                }
                if let Err(e) =
                    app_state.handle.send(Request::SetWindowFrame(wid, target_frame, txid, true))
                {
                    debug!(?wid, ?e, "Failed to send frame request for hidden window");
                    continue;
                }
            }

            if let Some(window) = reactor.state.windows.window_mut(wid) {
                window.frame_monotonic = target_frame;
            }
        }

        if animated_count > 0 {
            let low_power = power::is_low_power_mode_enabled();
            let layout_animate = reactor
                .layout_manager
                .layout_engine
                .layout_specific_animate_settings(space)
                .unwrap_or(reactor.config.settings.animate);
            // `is_resize` means a window REPORTED a size change, which happens both
            // when the user is dragging an edge and when we ourselves resized it via
            // a command (ctrl-R preset cycling, ctrl-F full width). Skipping the
            // animation is right for a drag — animation would lag the cursor — but
            // wrong for a command, and it produced a specific visible artefact:
            //
            //   the NEIGHBOUR column animated into its new position (that pass had
            //   is_resize = false), then the resized window SNAPPED to its new size
            //   when the app's own resize notification arrived and forced a second,
            //   unanimated pass.
            //
            // Only a real interactive drag should skip. is_in_drag() is the existing
            // signal for that, already used to suppress arrange passes mid-drag in
            // reactor.rs.
            let skip_anim = (is_resize && reactor.is_in_drag()) || !layout_animate || low_power;

            if let Some(tx) = &reactor.animation_tx {
                let message = if skip_anim {
                    Message::SkipToEnd(anim)
                } else {
                    Message::Replace(anim)
                };
                if let Err(err) = tx.send(message) {
                    match err.0 {
                        Message::Replace(animation) => animation.skip_to_end(),
                        Message::SkipToEnd(animation) => animation.skip_to_end(),
                    }
                }
            } else {
                anim.skip_to_end();
            }
        }

        any_frame_changed
    }

    pub fn instant_layout(
        reactor: &mut Reactor,
        space: SpaceId,
        layout: &[(WindowId, CGRect)],
        skip_wid: Option<WindowId>,
    ) -> bool {
        Self::instant_layout_inner(reactor, space, layout, skip_wid, false)
    }

    /// Apply the position-only layout used while switching virtual workspaces.
    ///
    /// Keep this entry point separate from `instant_layout`: layouts merely suppressed
    /// while a switch is in progress may still change window sizes and must use the
    /// full-frame request.
    pub fn workspace_switch_layout(
        reactor: &mut Reactor,
        space: SpaceId,
        layout: &[(WindowId, CGRect)],
        skip_wid: Option<WindowId>,
    ) -> bool {
        Self::instant_layout_inner(reactor, space, layout, skip_wid, true)
    }

    fn instant_layout_inner(
        reactor: &mut Reactor,
        space: SpaceId,
        layout: &[(WindowId, CGRect)],
        skip_wid: Option<WindowId>,
        position_only: bool,
    ) -> bool {
        let mut per_app: HashMap<pid_t, Vec<(WindowId, CGRect, bool)>> = HashMap::default();
        let mut any_frame_changed = false;

        for &(wid, target_frame) in layout {
            if skip_wid == Some(wid) {
                trace!(?wid, "Skipping layout update for window currently being dragged");
                continue;
            }

            let is_hidden = !reactor.layout_manager.layout_engine.is_window_in_active_workspace(
                &reactor.state.windows,
                space,
                wid,
            );
            let window_store = &mut reactor.state.windows;
            let Some(window) = window_store.window_mut(wid) else {
                debug!(?wid, "Skipping layout - window no longer exists");
                continue;
            };
            let target_frame = target_frame.round();
            let current_frame = window.frame_monotonic;
            if target_frame.same_as(current_frame) {
                continue;
            }
            if let Some(wsid) = window.info.sys_id {
                if reactor
                    .transaction_manager
                    .get_target_frame(wsid)
                    .is_some_and(|pending| pending.same_as(target_frame))
                {
                    trace!(?wid, ?target_frame, "Skipping redundant instant layout request");
                    continue;
                }
            }
            any_frame_changed = true;
            trace!(
                ?wid,
                ?current_frame,
                ?target_frame,
                hidden = is_hidden,
                "Instant workspace positioning"
            );

            let size_unchanged = current_frame.size.same_as(target_frame.size);
            per_app.entry(wid.pid).or_default().push((wid, target_frame, size_unchanged));
            window.frame_monotonic = target_frame;
        }

        for (pid, frames) in per_app {
            if frames.is_empty() {
                continue;
            }

            let Some(app_state) = reactor.app_manager.apps.get(&pid) else {
                debug!(?pid, "Skipping layout update for app - app no longer exists");
                continue;
            };

            let handle = app_state.handle.clone();

            let (first_wid, first_target, _) = frames[0];
            let mut txid = TransactionId::default();
            let mut has_txid = false;
            let mut txid_entries: Vec<(WindowServerId, TransactionId, CGRect)> = Vec::new();
            if let Some(window) = reactor.state.windows.window_mut(first_wid) {
                if let Some(wsid) = window.info.sys_id {
                    txid = reactor.transaction_manager.generate_next_txid(wsid);
                    has_txid = true;
                    txid_entries.push((wsid, txid, first_target));
                }
            }

            if has_txid {
                for (wid, frame, _) in frames.iter().skip(1) {
                    if let Some(w) = reactor.state.windows.window_mut(*wid)
                        && let Some(wsid) = w.info.sys_id
                    {
                        reactor.transaction_manager.set_last_sent_txid(wsid, txid);
                        txid_entries.push((wsid, txid, *frame));
                    }
                }
                reactor.transaction_manager.update_txid_entries(txid_entries);
            }

            let requests = if position_only {
                let mut positions = Vec::new();
                let mut full_frames = Vec::new();
                for (wid, frame, size_unchanged) in frames {
                    if size_unchanged {
                        positions.push((wid, frame.origin));
                    } else {
                        full_frames.push((wid, frame));
                    }
                }

                let mut requests = Vec::with_capacity(2);
                if !positions.is_empty() {
                    requests.push(Request::SetWorkspaceSwitchPositions(positions, txid, true));
                }
                if !full_frames.is_empty() {
                    requests.push(Request::SetBatchWindowFrame(full_frames, txid, true));
                }
                requests
            } else {
                vec![Request::SetBatchWindowFrame(
                    frames.into_iter().map(|(wid, frame, _)| (wid, frame)).collect(),
                    txid,
                    true,
                )]
            };
            for request in requests {
                if let Err(e) = handle.send(request) {
                    debug!(
                        ?pid,
                        ?e,
                        "Failed to send instant layout request - app may have quit"
                    );
                    break;
                }
            }
        }

        any_frame_changed
    }
}

impl ActiveAnimation {
    fn start(animation: Animation) -> Option<Self> {
        if animation.is_empty() {
            return None;
        }
        animation.begin();
        Some(Self { animation, next_frame: 1 })
    }

    fn replace_with(self, mut next: Animation) -> Self {
        let current = self.current_frames();
        let continuing = next.patch_starts_from(&current);
        next.begin_windows_not_in(&continuing);
        next.carry_over(self.animation, &current);
        Self { animation: next, next_frame: 1 }
    }

    fn send_next_frame(&mut self) {
        self.animation.send_frame(self.next_frame);
        self.next_frame += 1;
    }

    fn is_complete(&self) -> bool {
        self.next_frame > self.animation.frames
    }

    fn current_frames(&self) -> Vec<(WindowId, CGRect)> {
        let frame = self.next_frame.saturating_sub(1);
        self.animation
            .windows
            .iter()
            .map(|window| (window.wid, window.frame_after(frame, self.animation.frames)))
            .collect()
    }
}

impl Animation {
    pub fn new(config: Config) -> Self {
        //const FPS: f64 = 100.0;
        //const DURATION: f64 = 0.30;
        let interval = Duration::from_secs_f64(1.0 / config.settings.animation_fps);
        Self {
            interval,
            frames: (config.settings.animation_duration * config.settings.animation_fps).round()
                as u32,
            windows: vec![],
            handled_windows: vec![],
        }
    }

    pub fn add_window(
        &mut self,
        handle: &AppThreadHandle,
        wid: WindowId,
        start: CGRect,
        finish: CGRect,
        is_focus: bool,
        txid: TransactionId,
    ) {
        self.windows.push(AnimatedWindow {
            handle: handle.clone(),
            wid,
            start,
            finish,
            is_focus,
            txid,
        });
        self.mark_handled(wid);
    }

    fn mark_handled(&mut self, wid: WindowId) {
        if !self.handled_windows.contains(&wid) {
            self.handled_windows.push(wid);
        }
    }

    pub fn skip_to_end(&self) {
        for window in &self.windows {
            _ = window.handle.send(Request::SetWindowFrame(
                window.wid,
                window.finish,
                window.txid,
                true,
            ));
        }
    }

    pub fn is_empty(&self) -> bool {
        self.windows.is_empty()
    }

    fn begin(&self) {
        self.begin_windows_not_in(&[]);
    }

    fn begin_windows_not_in(&self, skip: &[WindowId]) {
        for window in &self.windows {
            if skip.contains(&window.wid) {
                continue;
            }
            _ = window.handle.send(Request::BeginWindowAnimation(window.wid));
            // No pre-sizing of the focused window here.
            //
            // This used to immediately push `size: finish.size` at `start.origin`
            // before the animation began, which is the same size-snap that
            // frame_after used to do — the window jumped to its final width while
            // still at its old position, then slid. With size now interpolated
            // across the animation, sending this would undo it on the first frame.
        }
    }

    fn finish_all(&self) {
        for window in &self.windows {
            _ = window.handle.send(Request::AnimationFrame {
                wid: window.wid,
                frame: window.finish,
                set_size: true,
                txid: window.txid,
            });
            _ = window.handle.send(Request::EndWindowAnimation(window.wid));
        }
    }

    fn send_frame(&self, frame: u32) {
        for window in &self.windows {
            // Send the interpolated size on EVERY frame.
            //
            // This used to compute a smooth rect and then discard the size:
            //     let set_size = frame * 2 == self.frames || frame == self.frames;
            //     if set_size { rect.size = window.finish.size; }
            // so size reached the app only at the halfway frame and the last frame,
            // jumping straight to the final value at both. Position was sent every
            // frame, so a resize looked like the neighbouring window sliding
            // smoothly while this one held its old size and then popped — a visible
            // tear between the two mid-transition, and a resize that appeared to
            // start late.
            //
            // frame_after is the single source of truth for the interpolated rect
            // (it blends origin and size on one eased curve), so use it here instead
            // of recomputing, and always ask the app to apply the size.
            let rect = window.frame_after(frame, self.frames);
            _ = window.handle.send(Request::AnimationFrame {
                wid: window.wid,
                frame: rect,
                set_size: true,
                txid: window.txid,
            });
        }
    }

    fn end(&self) {
        for window in &self.windows {
            _ = window.handle.send(Request::EndWindowAnimation(window.wid));
        }
    }

    fn patch_starts_from(&mut self, current_frames: &[(WindowId, CGRect)]) -> Vec<WindowId> {
        let mut continuing = Vec::new();
        for &(wid, current_frame) in current_frames {
            let Some(window) = self.windows.iter_mut().find(|window| window.wid == wid) else {
                continue;
            };
            window.start = current_frame;
            continuing.push(wid);
        }
        continuing
    }

    fn carry_over(&mut self, previous: Animation, current_frames: &[(WindowId, CGRect)]) {
        for mut window in previous.windows {
            let continues_in_replacement =
                self.windows.iter().any(|existing| existing.wid == window.wid);
            if self.handled_windows.contains(&window.wid) || continues_in_replacement {
                // Balance the BeginWindowAnimation of a window the replacement DROPS.
                //
                // `handled_windows` covers both windows the replacement animates (added via
                // add_window, which marks them handled) and windows it deliberately excludes
                // — a dragged window, or one positioned directly because it is on an
                // inactive workspace. Only the latter need ending here: the former keep
                // animating and get their end from `end()`.
                //
                // Leaking it is not cosmetic. BeginWindowAnimation deregisters
                // AXWindowMoved/AXWindowResized, and app.rs drops move notifications while
                // is_animating is set, so a window left in that state stops reporting its
                // position. A drag then produces no WindowFrameChanged events at all, no
                // drag session is created, and the layout pass keeps reasserting the stored
                // frame with nothing to suppress it — measured as has_session=false on
                // mouse-up for exactly this reason.
                if !continues_in_replacement {
                    _ = window.handle.send(Request::EndWindowAnimation(window.wid));
                }
                continue;
            }
            if let Some(&(_, current_frame)) =
                current_frames.iter().find(|(wid, _)| *wid == window.wid)
            {
                window.start = current_frame;
            }
            self.windows.push(window);
        }
    }

    fn skip_to_end_and_end(self) {
        self.finish_all();
    }
}

fn get_frame(a: CGRect, b: CGRect, t: f64) -> CGRect {
    let s = ease(t);
    CGRect {
        origin: CGPoint {
            x: blend(a.origin.x, b.origin.x, s),
            y: blend(a.origin.y, b.origin.y, s),
        },
        size: CGSize {
            width: blend(a.size.width, b.size.width, s),
            height: blend(a.size.height, b.size.height, s),
        },
    }
}

fn ease(t: f64) -> f64 {
    if t < 0.5 {
        (1.0 - f64::sqrt(1.0 - f64::powi(2.0 * t, 2))) / 2.0
    } else {
        (f64::sqrt(1.0 - f64::powi(-2.0 * t + 2.0, 2)) + 1.0) / 2.0
    }
}

fn blend(a: f64, b: f64, s: f64) -> f64 {
    (1.0 - s) * a + s * b
}

#[cfg(test)]
mod tests {
    use objc2_core_foundation::{CGPoint, CGSize};

    use super::*;

    fn rect(origin_x: f64, origin_y: f64, width: f64, height: f64) -> CGRect {
        CGRect::new(CGPoint::new(origin_x, origin_y), CGSize::new(width, height))
    }

    fn config() -> Config {
        Config::default()
    }

    fn animation(handle: &AppThreadHandle, wid: WindowId, from: CGRect, to: CGRect) -> Animation {
        let mut animation = Animation::new(config());
        animation.add_window(handle, wid, from, to, false, TransactionId::default());
        animation
    }

    fn collect_requests(rx: &mut crate::actor::Receiver<Request>) -> Vec<Request> {
        let mut requests = Vec::new();
        while let Ok((_, request)) = rx.try_recv() {
            requests.push(request);
        }
        requests
    }

    fn assert_set_window_frame(request: &Request, wid: WindowId, frame: CGRect) {
        match request {
            Request::SetWindowFrame(req_wid, req_frame, txid, eui) => {
                assert_eq!(*req_wid, wid);
                assert_eq!(*req_frame, frame);
                assert_eq!(*txid, TransactionId::default());
                assert!(*eui);
            }
            _ => panic!("expected SetWindowFrame, got {request:?}"),
        }
    }

    fn assert_animation_frame(request: &Request, wid: WindowId, frame: CGRect) {
        match request {
            Request::AnimationFrame {
                wid: req_wid,
                frame: req_frame,
                set_size,
                txid,
            } => {
                assert_eq!(*req_wid, wid);
                assert_eq!(*req_frame, frame);
                assert!(*set_size, "expected a set_size frame");
                assert_eq!(*txid, TransactionId::default());
            }
            _ => panic!("expected AnimationFrame, got {request:?}"),
        }
    }

    fn assert_animation_pos(request: &Request, wid: WindowId, pos: CGPoint) {
        match request {
            Request::AnimationFrame {
                wid: req_wid,
                frame,
                set_size,
                txid,
            } => {
                assert_eq!(*req_wid, wid);
                assert_eq!(frame.origin, pos);
                // set_size is now true on every frame: size is interpolated
                // alongside position rather than applied only at the midpoint and
                // the end. This helper only cares about the POSITION, which is what
                // its callers assert.
                assert_eq!(*txid, TransactionId::default());
            }
            _ => panic!("expected AnimationFrame, got {request:?}"),
        }
    }

    #[test]
    fn replacement_uses_last_animated_frame_for_continuing_windows() {
        let (tx, mut rx) = crate::actor::channel();
        let handle = AppThreadHandle::new_for_test(tx);
        let wid = WindowId::new(1, 1);
        let first = animation(
            &handle,
            wid,
            rect(0.0, 0.0, 10.0, 10.0),
            rect(50.0, 60.0, 10.0, 10.0),
        );
        let second = animation(
            &handle,
            wid,
            rect(50.0, 60.0, 10.0, 10.0),
            rect(80.0, 90.0, 10.0, 10.0),
        );

        let mut manager = AnimationManager::new();
        manager.handle_message(Message::Replace(first));
        assert!(matches!(
            collect_requests(&mut rx).as_slice(),
            [Request::BeginWindowAnimation(req_wid)] if *req_wid == wid
        ));

        manager.tick();
        let continuing_frame = manager.active.as_ref().unwrap().current_frames()[0].1;
        assert_animation_pos(&collect_requests(&mut rx)[0], wid, continuing_frame.origin);

        manager.handle_message(Message::Replace(second));
        assert!(collect_requests(&mut rx).is_empty());

        let resumed_start = manager.active.as_ref().unwrap().animation.windows[0].start;
        assert_eq!(resumed_start, continuing_frame);

        manager.tick();
        let expected_next = get_frame(resumed_start, rect(80.0, 90.0, 10.0, 10.0), 1.0 / 30.0);
        assert_animation_pos(&collect_requests(&mut rx)[0], wid, expected_next.origin);
    }

    fn animation_contains(manager: &AnimationManager, wid: WindowId) -> bool {
        manager
            .active
            .as_ref()
            .is_some_and(|active| active.animation.windows.iter().any(|w| w.wid == wid))
    }

    #[test]
    fn replacement_only_restarts_changed_windows() {
        let (tx, mut rx) = crate::actor::channel();
        let handle = AppThreadHandle::new_for_test(tx);
        let wid1 = WindowId::new(1, 1);
        let wid2 = WindowId::new(1, 2);
        let wid3 = WindowId::new(1, 3);
        let mut first = Animation::new(config());
        first.add_window(
            &handle,
            wid1,
            rect(0.0, 0.0, 10.0, 10.0),
            rect(50.0, 60.0, 10.0, 10.0),
            false,
            TransactionId::default(),
        );
        first.add_window(
            &handle,
            wid2,
            rect(10.0, 0.0, 10.0, 10.0),
            rect(60.0, 60.0, 10.0, 10.0),
            false,
            TransactionId::default(),
        );
        let mut second = Animation::new(config());
        second.add_window(
            &handle,
            wid1,
            rect(50.0, 60.0, 10.0, 10.0),
            rect(80.0, 90.0, 10.0, 10.0),
            false,
            TransactionId::default(),
        );
        second.add_window(
            &handle,
            wid3,
            rect(20.0, 0.0, 10.0, 10.0),
            rect(90.0, 90.0, 10.0, 10.0),
            false,
            TransactionId::default(),
        );

        let mut manager = AnimationManager::new();
        manager.handle_message(Message::Replace(first));
        assert_eq!(collect_requests(&mut rx).len(), 2);
        manager.handle_message(Message::Replace(second));

        let requests = collect_requests(&mut rx);
        assert_eq!(requests.len(), 1);
        assert!(matches!(requests[0], Request::BeginWindowAnimation(req_wid) if req_wid == wid3));
        assert!(animation_contains(&manager, wid2));

        let carried = manager
            .active
            .as_ref()
            .unwrap()
            .animation
            .windows
            .iter()
            .find(|w| w.wid == wid2)
            .unwrap();
        assert_eq!(carried.finish, rect(60.0, 60.0, 10.0, 10.0));
    }

    #[test]
    fn replacement_does_not_carry_over_explicitly_handled_windows() {
        let (tx, mut rx) = crate::actor::channel();
        let handle = AppThreadHandle::new_for_test(tx);
        let wid1 = WindowId::new(1, 1);
        let wid2 = WindowId::new(1, 2);
        let mut first = Animation::new(config());
        first.add_window(
            &handle,
            wid1,
            rect(0.0, 0.0, 10.0, 10.0),
            rect(50.0, 60.0, 10.0, 10.0),
            false,
            TransactionId::default(),
        );
        first.add_window(
            &handle,
            wid2,
            rect(10.0, 0.0, 10.0, 10.0),
            rect(60.0, 60.0, 10.0, 10.0),
            false,
            TransactionId::default(),
        );
        let mut second = Animation::new(config());
        second.add_window(
            &handle,
            wid1,
            rect(50.0, 60.0, 10.0, 10.0),
            rect(80.0, 90.0, 10.0, 10.0),
            false,
            TransactionId::default(),
        );
        second.mark_handled(wid2);

        let mut manager = AnimationManager::new();
        manager.handle_message(Message::Replace(first));
        let _ = collect_requests(&mut rx);
        manager.handle_message(Message::Replace(second));

        assert!(!animation_contains(&manager, wid2));
    }

    /// A window the replacement DROPS must have its animation ended.
    ///
    /// BeginWindowAnimation deregisters AXWindowMoved/AXWindowResized and sets
    /// is_animating, which makes app.rs drop move notifications. Leaking that leaves the
    /// window unable to report its position for the rest of the session: a drag produces no
    /// WindowFrameChanged events, no drag session is created, and the layout pass reasserts
    /// the stored frame with nothing to suppress it.
    #[test]
    fn replacement_ends_the_animation_of_a_window_it_drops() {
        let (tx, mut rx) = crate::actor::channel();
        let handle = AppThreadHandle::new_for_test(tx);
        let dragged = WindowId::new(1, 1);
        let continuing = WindowId::new(1, 2);
        let mut first = Animation::new(config());
        for wid in [dragged, continuing] {
            first.add_window(
                &handle,
                wid,
                rect(0.0, 0.0, 10.0, 10.0),
                rect(50.0, 60.0, 10.0, 10.0),
                false,
                TransactionId::default(),
            );
        }
        // The replacement animates `continuing` and excludes `dragged`, which is what
        // animate_layout does for a window the user has grabbed.
        let mut second = Animation::new(config());
        second.add_window(
            &handle,
            continuing,
            rect(50.0, 60.0, 10.0, 10.0),
            rect(80.0, 90.0, 10.0, 10.0),
            false,
            TransactionId::default(),
        );
        second.mark_handled(dragged);

        let mut manager = AnimationManager::new();
        manager.handle_message(Message::Replace(first));
        let _ = collect_requests(&mut rx);
        manager.handle_message(Message::Replace(second));

        let requests = collect_requests(&mut rx);
        assert!(
            requests.iter().any(
                |request| matches!(request, Request::EndWindowAnimation(wid) if *wid == dragged)
            ),
            "a dropped window must be released from animation state, else it stops \
             reporting its position entirely; got {requests:?}"
        );
        assert!(
            !requests.iter().any(
                |request| matches!(request, Request::EndWindowAnimation(wid) if *wid == continuing)
            ),
            "a window that keeps animating must NOT be ended mid-flight; got {requests:?}"
        );
    }

    #[test]
    fn skip_to_end_finishes_active_animation_and_applies_new_layout() {
        let (tx, mut rx) = crate::actor::channel();
        let handle = AppThreadHandle::new_for_test(tx);
        let wid = WindowId::new(1, 1);
        let first = animation(
            &handle,
            wid,
            rect(0.0, 0.0, 10.0, 10.0),
            rect(50.0, 60.0, 10.0, 10.0),
        );
        let second = animation(
            &handle,
            wid,
            rect(50.0, 60.0, 10.0, 10.0),
            rect(80.0, 90.0, 10.0, 10.0),
        );

        let mut manager = AnimationManager::new();
        manager.handle_message(Message::Replace(first));
        manager.handle_message(Message::SkipToEnd(second));

        let requests = collect_requests(&mut rx);
        assert_eq!(requests.len(), 4);
        assert!(matches!(requests[0], Request::BeginWindowAnimation(req_wid) if req_wid == wid));
        assert_animation_frame(&requests[1], wid, rect(50.0, 60.0, 10.0, 10.0));
        assert!(matches!(requests[2], Request::EndWindowAnimation(req_wid) if req_wid == wid));
        assert_set_window_frame(&requests[3], wid, rect(80.0, 90.0, 10.0, 10.0));
    }

    /// Size must be interpolated across the animation, not snapped.
    ///
    /// The focused window used to receive `finish.size` at frame 0 and every other
    /// window held `start.size` until halfway then jumped, while POSITION eased
    /// smoothly throughout. A resize therefore read as "the neighbour glides into
    /// place, then this window pops to its new width", with a visible gap in
    /// between.
    #[test]
    fn animation_interpolates_size_not_just_position() {
        let start = rect(0.0, 0.0, 100.0, 100.0);
        let finish = rect(200.0, 0.0, 400.0, 100.0);
        let (tx, _rx) = crate::actor::channel();
        let window = AnimatedWindow {
            handle: AppThreadHandle::new_for_test(tx),
            wid: WindowId::new(1, 1),
            start,
            finish,
            is_focus: true,
            txid: TransactionId::default(),
        };

        let total = 10u32;
        let mut widths = Vec::new();
        for frame in 0..=total {
            widths.push(window.frame_after(frame, total).size.width);
        }

        assert_eq!(
            widths[0], start.size.width,
            "frame 0 must start at the old width"
        );
        assert_eq!(
            widths[total as usize], finish.size.width,
            "final frame must reach the target width"
        );

        // Strictly increasing: a snap would show a run of identical values followed
        // by one jump.
        for pair in widths.windows(2) {
            assert!(pair[1] >= pair[0], "width must not go backwards: {:?}", widths);
        }
        let distinct = widths
            .iter()
            .map(|w| (w * 100.0).round() as i64)
            .collect::<std::collections::HashSet<_>>()
            .len();
        assert!(
            distinct > total as usize / 2,
            "expected a smooth ramp of widths, got {} distinct values in {:?}",
            distinct,
            widths
        );
    }

    /// Every animation frame must carry the interpolated SIZE to the app.
    ///
    /// send_frame used to compute a smooth rect and then overwrite the size with
    /// finish.size, flagging set_size only at the halfway frame and the last frame.
    /// Position was sent every frame, so a resize appeared to start late and tore
    /// against the neighbouring window that was already sliding.
    #[test]
    fn every_animation_frame_carries_an_interpolated_size() {
        let (tx, mut rx) = crate::actor::channel();
        let handle = AppThreadHandle::new_for_test(tx);
        let wid = WindowId::new(1, 1);
        let anim = animation(
            &handle,
            wid,
            rect(0.0, 0.0, 100.0, 100.0),
            rect(300.0, 0.0, 400.0, 100.0),
        );

        let mut manager = AnimationManager::new();
        manager.handle_message(Message::Replace(anim));
        let _ = collect_requests(&mut rx); // BeginWindowAnimation

        let mut widths = Vec::new();
        for _ in 0..6 {
            manager.tick();
            for request in collect_requests(&mut rx) {
                if let Request::AnimationFrame { frame, set_size, .. } = request {
                    assert!(set_size, "every frame must apply the size");
                    widths.push(frame.size.width);
                }
            }
        }

        assert!(widths.len() >= 3, "expected several frames, got {widths:?}");
        // Intermediate widths must lie strictly between start and finish, which is
        // what proves interpolation rather than a snap to one end or the other.
        let intermediate = widths.iter().filter(|w| **w > 100.5 && **w < 399.5).count();
        assert!(
            intermediate >= 2,
            "expected intermediate widths between 100 and 400, got {widths:?}"
        );
    }

    /// A pure scroll (position changes, size does not) must not request a resize on
    /// every frame.
    ///
    /// flush_frames in app.rs takes a three-AX-call path whenever a frame asks for a
    /// size change, and those calls are synchronous: paying it per window per frame
    /// makes each window's frame land later than the last, so windows animating
    /// together drift apart and tear. This asserts the frames a scroll produces all
    /// carry the SAME size, which is what lets the flush skip the expensive path.
    #[test]
    fn scrolling_frames_keep_a_constant_size() {
        let (tx, mut rx) = crate::actor::channel();
        let handle = AppThreadHandle::new_for_test(tx);
        let wid = WindowId::new(1, 1);
        // Same size at both ends, different position: this is a scroll.
        let anim = animation(
            &handle,
            wid,
            rect(0.0, 0.0, 400.0, 800.0),
            rect(900.0, 0.0, 400.0, 800.0),
        );

        let mut manager = AnimationManager::new();
        manager.handle_message(Message::Replace(anim));
        let _ = collect_requests(&mut rx);

        let mut sizes = Vec::new();
        let mut positions = Vec::new();
        for _ in 0..8 {
            manager.tick();
            for request in collect_requests(&mut rx) {
                if let Request::AnimationFrame { frame, .. } = request {
                    sizes.push((frame.size.width, frame.size.height));
                    positions.push(frame.origin.x);
                }
            }
        }

        assert!(sizes.len() >= 3, "expected several frames, got {sizes:?}");
        // 0.5pt tolerance, matching the tolerance flush_frames uses: blending
        // start and finish reintroduces float noise (400.00000000000006) even when
        // both ends are identical, and that must not count as a size change.
        assert!(
            sizes.iter().all(|(w, h)| (w - 400.0).abs() <= 0.5 && (h - 800.0).abs() <= 0.5),
            "a scroll must not change size across frames, got {sizes:?}"
        );
        // And it really is moving, so the test is not vacuous.
        assert!(
            positions.last().unwrap() > positions.first().unwrap(),
            "expected the window to travel, got {positions:?}"
        );
    }
}
