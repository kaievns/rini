//! The decorations at rest: a borderless, click-through, non-activating panel of rini's own directly
//! above each window whose bands show, drawing the bands a tile draws (`layers::Bands`). A
//! main-thread actor: the reactor tells it what each window wears, the window server where each
//! window is and when it was reordered, hidden or unhidden, and the renderer when its overlay lifts.
//! The rules are `domain::rest`; why panels are pooled, shown by alpha and ordered as they are is "At
//! rest" in `src/decorations/docs/README.md`.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Instant;

use objc2::rc::Retained;
use objc2::{MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{
    NSBackingStoreType, NSColor, NSPanel, NSScreen, NSView, NSWindowCollectionBehavior,
    NSWindowOrderingMode, NSWindowStyleMask,
};
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_quartz_core::{CALayer, CATransaction};
use rini_core::ids::WindowServerId;
use rini_geometry::space::{FromCg, Respace, ScreenRect, ToCg};
use rini_runloop::channel;
use rini_runloop::executor::sleep;
use tracing::warn;

use crate::decorations::domain::rest::{self, Rest, RestDisplay, RestModel, RestWindow, display_of};
use crate::decorations::domain::shape;
use crate::decorations::platform::layers::Bands;
use crate::displays::domain::screen::CoordinateConverter;
use crate::displays::event::{ServerFrame, ServerOrder};
use crate::displays::screen::primary_display_height;
use crate::windows::platform::window_server::{self, StackPlace};

pub type Sender = channel::Sender<Event>;
pub type Receiver = channel::Receiver<Event>;

pub enum Event {
    /// What every decorated window wears and what holds its band, sent when it changed.
    Model(RestModel),
    /// Where the window server has windows that moved or resized.
    Frames(Vec<ServerFrame>),
    /// A window the window server reordered, hid or unhid.
    Order(ServerOrder),
}

define_class!(
    /// Top-left origin, so a band is laid in the window server's coordinates.
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[name = "RiniRestView"]
    struct RestView;

    impl RestView {
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }
    }
);

define_class!(
    /// AppKit moves a window out of the menu bar's band; a band around a window at the top of the
    /// display reaches into it, so the frame is taken as given.
    #[unsafe(super(NSPanel))]
    #[thread_kind = MainThreadOnly]
    #[name = "RiniRestPanel"]
    struct RestPanel;

    impl RestPanel {
        #[unsafe(method(constrainFrameRect:toScreen:))]
        fn constrain_frame_rect(&self, frame: CGRect, _screen: Option<&NSScreen>) -> CGRect {
            frame
        }
    }
);

/// On the active Space, out of the window cycle and fullscreen, and hidden by Mission Control, App
/// Exposé and Show Desktop rather than laid out as a window of its own. Never `.stationary`: Exposé
/// leaves a stationary window where it is, over the thumbnails.
fn collection_behaviour() -> NSWindowCollectionBehavior {
    NSWindowCollectionBehavior::MoveToActiveSpace
        | NSWindowCollectionBehavior::Transient
        | NSWindowCollectionBehavior::IgnoresCycle
        | NSWindowCollectionBehavior::FullScreenNone
}

/// One rest window, kept for the life of the process once made.
struct Panel {
    window: Retained<RestPanel>,
    root: Retained<CALayer>,
    /// At the decorated window's frame, in the panel's points.
    holder: Retained<CALayer>,
    bands: Bands,
    target: Option<WindowServerId>,
    /// Its frame on screen as last set, and whether it was shown.
    placed: Option<(ScreenRect, bool)>,
    /// Its target's corner radius as the window server reports it, read when it was given the
    /// target; a window rule's goes ahead of it.
    radius: Option<f64>,
}

impl Panel {
    fn new(mtm: MainThreadMarker) -> Option<Panel> {
        let frame = CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(1.0, 1.0));
        // SAFETY: AppKit's designated initializer, on the main thread.
        let window: Retained<RestPanel> = unsafe {
            msg_send![
                RestPanel::alloc(mtm),
                initWithContentRect: frame,
                styleMask: NSWindowStyleMask::Borderless | NSWindowStyleMask::NonactivatingPanel,
                backing: NSBackingStoreType::Buffered,
                defer: false,
            ]
        };
        window.setOpaque(false);
        window.setBackgroundColor(Some(&NSColor::clearColor()));
        window.setHasShadow(false);
        window.setIgnoresMouseEvents(true);
        window.setBecomesKeyOnlyIfNeeded(true);
        window.setHidesOnDeactivate(false);
        window.setCanHide(false);
        window.setCollectionBehavior(collection_behaviour());
        window.setAlphaValue(0.0);
        // SAFETY: a fresh view, initialised as AppKit's designated initializer requires.
        let view: Retained<RestView> = unsafe { msg_send![RestView::alloc(mtm), initWithFrame: frame] };
        view.setWantsLayer(true);
        window.setContentView(Some(&view));
        let root = view.layer()?;
        let holder = CALayer::layer();
        holder.setAnchorPoint(CGPoint::new(0.0, 0.0));
        root.addSublayer(&holder);
        let bands = Bands::new(&holder);
        // Ordered in once, and only shown or hidden by alpha from here: ordering costs a frame.
        window.orderFrontRegardless();
        Some(Panel { window, root, holder, bands, target: None, placed: None, radius: None })
    }

    fn own_id(&self) -> Option<WindowServerId> {
        u32::try_from(self.window.windowNumber()).ok().filter(|id| *id > 0).map(WindowServerId::new)
    }

    /// Directly above `target`: at its level and sub-level, then ordered above it.
    fn order_above(&self, target: WindowServerId, frame: ScreenRect) {
        let Some(own) = self.own_id() else { return };
        window_server::take_stack_place(own, &StackPlace::of(target, frame.to_cg()));
        self.window.orderWindow_relativeTo(NSWindowOrderingMode::Above, target.as_u32() as isize);
    }

    /// Draws `window`'s bands around its window at `frame` on `display`, or none.
    fn place(&mut self, window: &RestWindow, frame: ScreenRect, display: RestDisplay, showing: bool) {
        if let Some(outer) = self.lay(window, frame, display, showing) {
            self.reveal(outer);
        }
    }

    /// Lays its layers for `window`'s bands around its window at `frame`, and the rest window's frame
    /// they want; hides it when none shows.
    fn lay(&mut self, window: &RestWindow, frame: ScreenRect, display: RestDisplay, showing: bool) -> Option<ScreenRect> {
        let outset = window.outset(display.scale);
        let Some(outer) = shape::rest_frame(frame, outset, display.bounds).filter(|_| showing) else {
            self.hide();
            return None;
        };
        let inside = frame.origin - outer.origin;
        self.holder.setFrame(CGRect::new(CGPoint::new(inside.x, inside.y), frame.size.to_cg()));
        self.root.setContentsScale(display.scale);
        let size = frame.size.respace();
        self.bands.wear(&window.decoration, display.scale);
        self.bands.place(size, shape::radius(window.decoration.corner(|| self.radius), size));
        self.bands.paint(&window.style());
        Some(outer)
    }

    /// At `outer`, shown.
    fn reveal(&mut self, outer: ScreenRect) {
        if self.placed.is_none_or(|(at, _)| at != outer) {
            let converter = CoordinateConverter::from_height(primary_display_height());
            if let Some(cocoa) = converter.reframe(outer.to_cg(), self.window.frame()) {
                self.window.setFrame_display(cocoa, false);
            }
        }
        if self.placed.is_none_or(|(_, shown)| !shown) {
            self.window.setAlphaValue(1.0);
        }
        self.placed = Some((outer, true));
    }

    fn shown(&self) -> bool { self.placed.is_some_and(|(_, shown)| shown) }

    fn hide(&mut self) {
        if self.placed.is_none_or(|(_, shown)| shown) {
            self.window.setAlphaValue(0.0);
        }
        self.placed = self.placed.map(|(at, _)| (at, false));
    }
}

/// Every rest window, and what it is drawn from.
pub struct RestBorders {
    mtm: MainThreadMarker,
    rest: Rest,
    panels: Vec<Panel>,
    /// Whether the one failure to make a panel has been logged.
    warned: bool,
}

impl RestBorders {
    fn new(mtm: MainThreadMarker) -> RestBorders {
        RestBorders { mtm, rest: Rest::default(), panels: Vec::new(), warned: false }
    }

    /// Every event queued, taken as one turn: the latest model, every window's latest frame, and each
    /// rest window ordered again at most once.
    fn take(&mut self, events: Vec<Event>, now: Instant) {
        let mut orders = Vec::new();
        let mut moved: Vec<ServerFrame> = Vec::new();
        let mut model = None;
        for event in events {
            match event {
                Event::Model(next) => model = Some(next),
                Event::Frames(frames) => moved.extend(frames),
                Event::Order(order) => orders.push(order),
            }
        }
        let mut also = Vec::new();
        if let Some(model) = model {
            let read = |window| {
                let frame = window_server::server_frame(window).map(ScreenRect::from_cg);
                (frame, window_server::window_ordered_in(window) == Some(false))
            };
            let focus = self.rest.take_model(model, now, read);
            for _ in self.panels.len()..self.rest.pool_size() {
                if !self.make_panel() {
                    break;
                }
            }
            also.extend(focus.and_then(|focus| self.panels.iter().position(|panel| panel.target == Some(focus))));
        }
        let moved: Vec<(WindowServerId, ScreenRect)> =
            moved.iter().map(|frame| (frame.window, ScreenRect::from_cg(frame.frame))).collect();
        self.rest.take_frames(&moved, now);
        self.rest.take_orders(&orders);
        self.draw(&orders, also);
    }

    /// The overlay lifts off `display`: every park waiting there taken, each window the domain names
    /// read again from the window server, and every band drawn as it is now, before the overlay hides.
    fn lift(&mut self, display: ScreenRect, now: Instant) {
        let panels = &self.panels;
        let reread = self.rest.lift(display, now, |window| panels.iter().any(|panel| panel.target == Some(window)));
        for window in reread {
            self.rest.reread(window, window_server::server_frame(window).map(ScreenRect::from_cg));
        }
        self.draw(&[], Vec::new());
    }

    fn settle(&mut self, now: Instant) {
        self.rest.settle(now);
        self.draw(&[], Vec::new());
    }

    /// One rest window for each window whose band shows: those `orders` and `also` name, and those
    /// just given a window, ordered above their windows, then every one placed and painted in one
    /// transaction. One showing a window afresh (`rest::afresh`) is hidden first, and takes its frame,
    /// its place in the stack and alpha 1 only once its layers are committed.
    fn draw(&mut self, orders: &[ServerOrder], mut also: Vec<usize>) {
        let displays = self.rest.model.displays.clone();
        let wanted = self.rest.wanted();
        let pool: Vec<Option<WindowServerId>> = self.panels.iter().map(|panel| panel.target).collect();
        let assignment = rest::assign(&pool, &wanted);
        let mut given = Vec::new();
        for (at, window) in assignment.retargeted {
            let panel = &mut self.panels[at];
            panel.target = Some(window);
            panel.radius = window_server::corner_radii(&[window]).get(&window).copied();
            given.push(at);
        }
        also.extend(&given);
        let before: Vec<(Option<WindowServerId>, bool)> =
            self.panels.iter().map(|panel| (panel.target, panel.shown())).collect();
        let afresh = rest::afresh(&before, &given, &wanted);
        let targets: Vec<Option<WindowServerId>> = before.iter().map(|(target, _)| *target).collect();
        let reorder = rest::to_reorder(orders, &targets, &also);
        for at in reorder.iter().filter(|at| !afresh.contains(at)) {
            self.order_above_its_window(*at);
        }
        for at in &afresh {
            self.panels[*at].hide();
        }
        let mut revealed = Vec::new();
        CATransaction::begin();
        CATransaction::setDisableActions(true);
        for at in assignment.released {
            self.panels[at].target = None;
            self.panels[at].hide();
        }
        for (at, panel) in self.panels.iter_mut().enumerate() {
            let Some(target) = panel.target else { continue };
            let window = self.rest.model.windows.iter().find(|w| w.window == target);
            let frame = self.rest.windows.get(&target).and_then(|tracked| tracked.frame);
            let Some((window, frame, display)) =
                window.zip(frame).and_then(|(window, frame)| Some((window, frame, display_of(frame, &displays)?)))
            else {
                panel.hide();
                continue;
            };
            let showing = wanted.contains(&target);
            if afresh.contains(&at) {
                revealed.extend(panel.lay(window, frame, display, showing).map(|outer| (at, outer)));
            } else {
                panel.place(window, frame, display, showing);
            }
        }
        CATransaction::commit();
        CATransaction::flush();
        for (at, outer) in revealed {
            self.panels[at].reveal(outer);
            self.order_above_its_window(at);
        }
    }

    fn order_above_its_window(&self, at: usize) {
        let panel = &self.panels[at];
        let target = panel.target.and_then(|target| Some((target, self.rest.windows.get(&target)?.frame?)));
        if let Some((target, frame)) = target {
            panel.order_above(target, frame);
        }
    }

    /// One more rest window in the pool; false when AppKit could not make one.
    fn make_panel(&mut self) -> bool {
        match Panel::new(self.mtm) {
            Some(panel) => {
                self.panels.push(panel);
                true
            }
            None => {
                if !self.warned {
                    warn!("could not create a rest window; bands at rest will be missing");
                    self.warned = true;
                }
                false
            }
        }
    }
}

/// What the renderer calls as it lifts the overlay off a display, before it hides.
pub type OnLift = Box<dyn Fn(ScreenRect)>;

pub struct RestActor {
    rx: Receiver,
    borders: Rc<RefCell<RestBorders>>,
}

impl RestActor {
    /// The actor, and what the renderer calls at each lift.
    pub fn new(rx: Receiver, mtm: MainThreadMarker) -> (RestActor, OnLift) {
        let borders = Rc::new(RefCell::new(RestBorders::new(mtm)));
        let at_lift = borders.clone();
        let on_lift: OnLift = Box::new(move |display| at_lift.borrow_mut().lift(display, Instant::now()));
        (RestActor { rx, borders }, on_lift)
    }

    pub async fn run(mut self) {
        loop {
            let due = self.borders.borrow().rest.due();
            let wait = due.map(|due| due.saturating_duration_since(Instant::now()));
            tokio::select! {
                woke = self.rx.recv() => {
                    let Some((_span, first)) = woke else { break };
                    let mut events = vec![first];
                    while let Ok((_, event)) = self.rx.try_recv() {
                        events.push(event);
                    }
                    self.borders.borrow_mut().take(events, Instant::now());
                }
                _ = sleep(wait.unwrap_or_default()), if wait.is_some() => {
                    self.borders.borrow_mut().settle(Instant::now());
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_rest_window_follows_the_active_space_and_takes_no_place_in_expose() {
        let behaviour = collection_behaviour();
        assert!(behaviour.contains(NSWindowCollectionBehavior::MoveToActiveSpace | NSWindowCollectionBehavior::Transient));
        assert!(!behaviour.intersects(NSWindowCollectionBehavior::Stationary | NSWindowCollectionBehavior::Managed));
    }
}
