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
use rustc_hash::FxHashMap as HashMap;
use tracing::warn;

use crate::decorations::domain::rest::{
    self, Parking, RestDisplay, RestModel, RestWindow, display_of, ordered_out, shown,
};
use crate::decorations::domain::shape;
use crate::decorations::platform::layers::Bands;
use crate::displays::domain::frames;
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
        window.setCollectionBehavior(
            NSWindowCollectionBehavior::MoveToActiveSpace
                | NSWindowCollectionBehavior::IgnoresCycle
                | NSWindowCollectionBehavior::FullScreenNone,
        );
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
        let outset = window.outset(display.scale);
        let Some(outer) = shape::rest_frame(frame, outset, display.bounds).filter(|_| showing) else {
            self.hide();
            return;
        };
        if self.placed.is_none_or(|(at, _)| at != outer) {
            let converter = CoordinateConverter::from_height(primary_display_height());
            if let Some(cocoa) = converter.reframe(outer.to_cg(), self.window.frame()) {
                self.window.setFrame_display(cocoa, false);
            }
        }
        let inside = frame.origin - outer.origin;
        self.holder.setFrame(CGRect::new(CGPoint::new(inside.x, inside.y), frame.size.to_cg()));
        self.root.setContentsScale(display.scale);
        let size = frame.size.respace();
        self.bands.wear(&window.decoration, display.scale);
        self.bands.place(size, shape::radius(window.decoration.corner(|| self.radius), size));
        self.bands.paint(&window.style());
        if self.placed.is_none_or(|(_, shown)| !shown) {
            self.window.setAlphaValue(1.0);
        }
        self.placed = Some((outer, true));
    }

    fn hide(&mut self) {
        if self.placed.is_none_or(|(_, shown)| shown) {
            self.window.setAlphaValue(0.0);
        }
        self.placed = self.placed.map(|(at, _)| (at, false));
    }
}

/// What is known of each decorated window between the reactor's words.
struct Tracked {
    frame: Option<ScreenRect>,
    parking: Parking,
    ordered_out: bool,
}

/// Every rest window, and what it is drawn from.
pub struct RestBorders {
    mtm: MainThreadMarker,
    model: RestModel,
    windows: HashMap<WindowServerId, Tracked>,
    panels: Vec<Panel>,
    /// Whether the one failure to make a panel has been logged.
    warned: bool,
}

impl RestBorders {
    fn new(mtm: MainThreadMarker) -> RestBorders {
        RestBorders { mtm, model: RestModel::default(), windows: HashMap::default(), panels: Vec::new(), warned: false }
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
            also.extend(self.take_model(model, now));
        }
        let ids: Vec<u32> = moved.iter().map(|frame| frame.window.as_u32()).collect();
        for id in frames::latest(&ids) {
            let window = WindowServerId::new(id);
            let Some(frame) = moved.iter().rev().find(|frame| frame.window == window) else { continue };
            if let Some(tracked) = self.windows.get_mut(&window) {
                tracked.frame = Some(ScreenRect::from_cg(frame.frame));
                tracked.parking.settle(true, false, now);
            }
        }
        for (window, tracked) in self.windows.iter_mut() {
            tracked.ordered_out = ordered_out(tracked.ordered_out, *window, &orders);
        }
        self.draw(&orders, also);
    }

    /// The reactor's word: windows new to it are read off the window server, its parks wait for
    /// their moves, and the rest window of a window focus came to is ordered again.
    fn take_model(&mut self, model: RestModel, now: Instant) -> Vec<usize> {
        let focus_before = self.model.windows.iter().find(|window| window.focused).map(|window| window.window);
        self.windows.retain(|window, _| model.windows.iter().any(|w| w.window == *window));
        for window in &model.windows {
            let tracked = self.windows.entry(window.window).or_insert_with(|| Tracked {
                frame: window_server::server_frame(window.window).map(ScreenRect::from_cg),
                parking: Parking::new(window.held.parked),
                ordered_out: window_server::window_ordered_in(window.window) == Some(false),
            });
            tracked.parking.want(window.held.parked, now);
        }
        let focus = model.windows.iter().find(|window| window.focused).map(|window| window.window);
        self.model = model;
        let focused_panel = focus
            .filter(|focus| Some(*focus) != focus_before)
            .and_then(|focus| self.panels.iter().position(|panel| panel.target == Some(focus)));
        focused_panel.into_iter().collect()
    }

    /// The overlay lifts off `display`: every park that waited taken, every rest window there and
    /// every window brought back at its frame as the window server has it, and every band drawn as
    /// it is now, before the overlay hides.
    fn lift(&mut self, display: ScreenRect, now: Instant) {
        for (window, tracked) in self.windows.iter_mut() {
            let parked = tracked.parking.parked;
            tracked.parking.settle(false, true, now);
            let brought_back = parked && !tracked.parking.parked;
            let drawn_there = tracked.frame.is_some_and(|frame| frame.intersects(&display))
                && self.panels.iter().any(|panel| panel.target == Some(*window));
            if brought_back || drawn_there {
                tracked.frame = window_server::server_frame(*window).map(ScreenRect::from_cg).or(tracked.frame);
            }
        }
        self.draw(&[], Vec::new());
    }

    /// Parks whose patience ran out, taken.
    fn settle(&mut self, now: Instant) {
        for tracked in self.windows.values_mut() {
            tracked.parking.settle(false, false, now);
        }
        self.draw(&[], Vec::new());
    }

    /// When the next waiting park takes effect.
    fn due(&self) -> Option<Instant> { self.windows.values().filter_map(|tracked| tracked.parking.due()).min() }

    /// One rest window for each window whose band shows: those `orders` and `also` name, and those
    /// just given a window, ordered above their windows, then every one placed and painted in one
    /// transaction.
    fn draw(&mut self, orders: &[ServerOrder], mut also: Vec<usize>) {
        let displays = self.model.displays.clone();
        let wanted: Vec<WindowServerId> = self
            .model
            .windows
            .iter()
            .filter(|window| {
                self.windows.get(&window.window).is_some_and(|tracked| {
                    shown(window, tracked.parking.parked, tracked.ordered_out, tracked.frame, &displays)
                })
            })
            .map(|window| window.window)
            .collect();
        for _ in 0..rest::spare(self.panels.len(), displays.len()) {
            self.make_panel();
        }
        let pool: Vec<Option<WindowServerId>> = self.panels.iter().map(|panel| panel.target).collect();
        let assignment = rest::assign(&pool, &wanted);
        let mut retargeted = assignment.retargeted;
        for window in assignment.made {
            if self.make_panel() {
                retargeted.push((self.panels.len() - 1, window));
            }
        }
        for (at, window) in retargeted {
            let panel = &mut self.panels[at];
            panel.target = Some(window);
            panel.radius = window_server::corner_radii(&[window]).get(&window).copied();
            also.push(at);
        }
        let targets: Vec<Option<WindowServerId>> = self.panels.iter().map(|panel| panel.target).collect();
        for at in rest::to_reorder(orders, &targets, &also) {
            let panel = &self.panels[at];
            let target = panel.target.and_then(|target| Some((target, self.windows.get(&target)?.frame?)));
            if let Some((target, frame)) = target {
                panel.order_above(target, frame);
            }
        }
        CATransaction::begin();
        CATransaction::setDisableActions(true);
        for at in assignment.released {
            self.panels[at].target = None;
            self.panels[at].hide();
        }
        for panel in self.panels.iter_mut() {
            let Some(target) = panel.target else { continue };
            let window = self.model.windows.iter().find(|w| w.window == target);
            let frame = self.windows.get(&target).and_then(|tracked| tracked.frame);
            match (window, frame, frame.and_then(|frame| display_of(frame, &displays))) {
                (Some(window), Some(frame), Some(display)) => {
                    panel.place(window, frame, display, wanted.contains(&target))
                }
                _ => panel.hide(),
            }
        }
        CATransaction::commit();
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
            let due = self.borders.borrow().due();
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
