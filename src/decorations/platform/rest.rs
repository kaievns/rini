//! The decorations at rest: a borderless, click-through, non-activating panel of rini's own directly
//! above each window whose bands show, drawing the bands a tile draws (`layers::Bands`). A
//! main-thread actor: the reactor tells it what each window wears and when a flight begins, the
//! window server, read on the frames thread, each window it asks of it as the model first names it,
//! where each window is and when it was reordered, hidden or unhidden, and the renderer when its
//! overlay goes up, when its flight has landed and when it lifts. It reads the window server itself
//! only as a landed flight places it, each frame drawn there. While no window can wear a band the reactor turns it
//! off: every rest window is closed, and it does nothing until the next model.
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
use rini_geometry::space::{FromCg, ScreenRect, ToCg};
use rini_runloop::executor::sleep;
use rini_runloop::{channel, turns};
use tracing::warn;

use crate::decorations::domain::rest::{
    self, Batch, Draw, Heard, Introduction, Layers, Look, PanelOp, PanelState, Placed, Rest,
    RestModel, Taken, Word, delta, stack_change,
};
use crate::decorations::platform::layers::Bands;
use crate::displays::domain::screen::CoordinateConverter;
use crate::displays::event::{Following, ServerRead, StackLevel, ToRead};
use crate::displays::screen::primary_display_height;
use crate::windows::platform::window_server;

pub type Sender = channel::Sender<Event>;
pub type Receiver = channel::Receiver<Event>;

pub enum Event {
    /// A flight over this display, its whole bounds, for the press at `pressed`, was handed to the
    /// flight engine: sent before the model of the batch that began it, so the rest windows hold that
    /// model and what follows from the begin to its lift (`Rest::fly`).
    Flight {
        display: ScreenRect,
        pressed: Instant,
    },
    /// What every decorated window wears and what holds its band, sent when it changed.
    Model(RestModel),
    /// What the frames thread read of the window server: where windows that moved or resized are, a
    /// window it reordered, hid, unhid or closed, or one introduced.
    Server(ServerRead),
    /// The overlay has lifted and wants a turn of the actor's: the pool owes rest windows the model
    /// wants, made now that it is down, or a park waits for its patience.
    Lifted,
    /// No window can wear a band: every rest window is closed, and nothing is taken until a model.
    Off,
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

/// One rest window, kept until the rest windows are turned off, and closed as it is dropped.
struct Panel {
    window: Retained<RestPanel>,
    root: Retained<CALayer>,
    /// At the decorated window's frame, in the panel's points.
    holder: Retained<CALayer>,
    bands: Bands,
    target: Option<WindowServerId>,
    /// What it last drew, and for which window; none since it was made, at alpha 0.
    look: Option<Look>,
    drew: Option<WindowServerId>,
    /// The level and sub-level it last took.
    level: Option<StackLevel>,
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
        // SAFETY: retained here, so AppKit must not release it again when `Drop` closes it.
        unsafe { window.setReleasedWhenClosed(false) };
        window.setAlphaValue(0.0);
        // SAFETY: a fresh view, initialised as AppKit's designated initializer requires.
        let view: Retained<RestView> =
            unsafe { msg_send![RestView::alloc(mtm), initWithFrame: frame] };
        view.setWantsLayer(true);
        window.setContentView(Some(&view));
        let root = view.layer()?;
        let holder = CALayer::layer();
        holder.setAnchorPoint(CGPoint::new(0.0, 0.0));
        root.addSublayer(&holder);
        let bands = Bands::new(&holder);
        // Ordered in once, and only shown or hidden by alpha from here: ordering costs a frame.
        window.orderFrontRegardless();
        turns::changed("rest.open");
        Some(Panel {
            window,
            root,
            holder,
            bands,
            target: None,
            look: None,
            drew: None,
            level: None,
        })
    }

    fn own_id(&self) -> Option<WindowServerId> {
        u32::try_from(self.window.windowNumber())
            .ok()
            .filter(|id| *id > 0)
            .map(WindowServerId::new)
    }

    /// Directly above `target`: at its `level` and sub-level, taken when it holds another, then
    /// ordered above it.
    fn order_above(&mut self, target: WindowServerId, level: StackLevel) {
        let Some(own) = self.own_id() else { return };
        if let Some(level) = stack_change(self.level, level) {
            window_server::take_stack_place(own, level.level, level.sub_level);
            self.level = Some(level);
        }
        self.window
            .orderWindow_relativeTo(NSWindowOrderingMode::Above, target.as_u32() as isize);
        turns::changed("rest.order");
    }

    /// Lays its layers for `now`.
    fn lay(&mut self, now: &Look) {
        let Layers {
            decoration,
            style,
            size,
            corner,
            scale,
            inside,
        } = now.layers;
        self.holder
            .setFrame(CGRect::new(CGPoint::new(inside.x, inside.y), size.to_cg()));
        self.root.setContentsScale(scale);
        self.bands.wear(&decoration, scale);
        self.bands.place(size, corner);
        self.bands.paint(&style);
    }

    /// Shown at `now`'s frame, each set only when it differs from what it drew.
    fn reveal(&mut self, now: Look) {
        let change = delta(self.look, now);
        if change.frame {
            let converter = CoordinateConverter::from_height(primary_display_height());
            if let Some(cocoa) = converter.reframe(now.outer.to_cg(), self.window.frame()) {
                self.window.setFrame_display(cocoa, false);
                turns::changed("rest.frame");
            }
        }
        if change.alpha {
            self.window.setAlphaValue(1.0);
            turns::changed("rest.show");
        }
        self.look = Some(now);
        self.drew = self.target;
    }

    fn state(&self) -> PanelState {
        PanelState {
            target: self.target,
            drew: self.drew,
            look: self.look,
        }
    }

    fn hide(&mut self) {
        let Some(was) = self.look else { return };
        let now = was.hidden();
        if delta(Some(was), now).alpha {
            self.window.setAlphaValue(0.0);
            turns::changed("rest.hide");
        }
        self.look = Some(now);
    }
}

impl Drop for Panel {
    fn drop(&mut self) {
        self.window.close();
        turns::changed("rest.close");
    }
}

/// Every rest window, and what it is drawn from.
pub struct RestBorders {
    mtm: MainThreadMarker,
    rest: Rest,
    /// Where a window the model first names is asked of the window server.
    following: Following,
    panels: Vec<Panel>,
    /// Whether the one failure to make a panel has been logged.
    warned: bool,
}

impl RestBorders {
    fn new(mtm: MainThreadMarker, following: Following) -> RestBorders {
        RestBorders {
            mtm,
            rest: Rest::default(),
            following,
            panels: Vec::new(),
            warned: false,
        }
    }

    /// Every event queued, taken as one turn (`Rest::take`): each window the latest model first
    /// names asked of the window server, the pool grown and the turn drawn; turned off by the latest
    /// word, every rest window closed.
    fn take(&mut self, events: Vec<Event>, now: Instant) {
        match self.rest.take(Batch::of(events.into_iter().map(heard)), now) {
            Taken::Off => self.panels.clear(),
            Taken::On { ask, grow, draw } => {
                for window in ask {
                    self.following.ask(ToRead::Intro(window));
                }
                if grow {
                    self.grow();
                }
                if let Some(draw) = draw {
                    self.draw(&draw);
                }
            }
        }
    }

    /// Rest windows made until the pool holds what the model wants, or AppKit makes none.
    fn grow(&mut self) {
        for _ in self.panels.len()..self.rest.pool_size() {
            if !self.make_panel() {
                break;
            }
        }
    }

    /// The flight over `display`, the one the overlay was laid out on if any, through the press at
    /// `through`, has landed: what waited under it taken, each window the domain names read again
    /// from the window server, and every band drawn as it is now, under the overlay, a tick before
    /// it lifts or in its lift's drain.
    fn place(&mut self, display: Option<ScreenRect>, through: Option<Instant>, now: Instant) {
        let panels = &self.panels;
        let drawn = |window| panels.iter().any(|panel| panel.target == Some(window));
        let Placed { draw, reread } = self.rest.place(display, through, now, drawn);
        if !self.rest.on() {
            return;
        }
        for window in reread {
            self.rest.reread(
                window,
                window_server::server_frame(window).map(ScreenRect::from_cg),
            );
        }
        self.draw(&draw);
    }

    /// The overlay lifts off `display`: what came since the place drawn where it changed, reading
    /// nothing, before the overlay hides, and every turn draws as it comes from here. Whether the
    /// actor wants a turn of its own once it is down (`Rest::wants_turn`): the pool owes rest windows,
    /// or a park waits for its patience.
    fn lift(
        &mut self,
        display: Option<ScreenRect>,
        through: Option<Instant>,
        now: Instant,
    ) -> bool {
        let draw = self.rest.lift(display, through, now);
        if !self.rest.on() {
            return false;
        }
        self.draw(&draw);
        self.rest.wants_turn(self.panels.len())
    }

    /// What ran out of patience: a flight that never showed lets go, the model that waited takes the
    /// rest windows it wants, now uncovered, and what waited is drawn; or a park takes effect.
    fn settle(&mut self, now: Instant) {
        if let Some(draw) = self.rest.settle(now) {
            self.grow();
            self.draw(&draw);
        }
    }

    /// `draw` carried out as `rest::plan_draw` orders it.
    fn draw(&mut self, draw: &Draw) {
        let panels: Vec<PanelState> = self.panels.iter().map(Panel::state).collect();
        for op in rest::plan_draw(&self.rest, &panels, draw) {
            match op {
                PanelOp::Target { at, window } => self.panels[at].target = window,
                PanelOp::Order { at, window, level } => self.panels[at].order_above(window, level),
                PanelOp::Hide { at } => self.panels[at].hide(),
                PanelOp::Begin => {
                    CATransaction::begin();
                    CATransaction::setDisableActions(true);
                }
                PanelOp::Lay { at, look } => self.panels[at].lay(&look),
                PanelOp::Reveal { at, look } => self.panels[at].reveal(look),
                PanelOp::Commit => {
                    CATransaction::commit();
                    CATransaction::flush();
                }
            }
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

/// `event` in the domain's spaces.
fn heard(event: Event) -> Heard {
    match event {
        Event::Flight { display, pressed } => Heard::Flight { display, pressed },
        Event::Model(model) => Heard::Word(Word::Model(model)),
        Event::Off => Heard::Word(Word::Off),
        Event::Server(ServerRead::Frames(frames)) => Heard::Frames(
            frames
                .iter()
                .map(|frame| (frame.window, ScreenRect::from_cg(frame.frame)))
                .collect(),
        ),
        Event::Server(ServerRead::Order(order)) => Heard::Order(order),
        Event::Server(ServerRead::Intro(intro)) => Heard::Intro(intro.window, Introduction {
            frame: intro.frame.map(ScreenRect::from_cg),
            ordered_out: intro.ordered_in == Some(false),
            radius: intro.radius,
            level: intro.level,
        }),
        Event::Lifted => Heard::Lifted,
    }
}

/// What the renderer calls on the main thread as its overlay goes up, as its flight has landed and
/// as it lifts, each with the display it was laid out on, if any, and the place and the lift with
/// the latest press the engine had taken when it gave them: the place before its drain's
/// transaction, the lift before it hides.
pub struct Handover {
    pub cover: Box<dyn Fn(Option<ScreenRect>)>,
    pub place: Box<dyn Fn(Option<ScreenRect>, Option<Instant>)>,
    pub lift: Box<dyn Fn(Option<ScreenRect>, Option<Instant>)>,
}

pub struct RestActor {
    rx: Receiver,
    borders: Rc<RefCell<RestBorders>>,
}

impl RestActor {
    /// The actor, and what the renderer calls as its overlay goes up, its flight lands and it lifts.
    /// `wake` is its own inbox, told once a lift wants a turn of it; `following` is where it asks the
    /// window server of a window first named.
    pub fn new(
        rx: Receiver,
        wake: Sender,
        following: Following,
        mtm: MainThreadMarker,
    ) -> (RestActor, Handover) {
        let borders = Rc::new(RefCell::new(RestBorders::new(mtm, following)));
        let covered = borders.clone();
        let placed = borders.clone();
        let lifted = borders.clone();
        let handover = Handover {
            cover: Box::new(move |display| covered.borrow_mut().rest.cover(display)),
            place: Box::new(move |display, through| {
                placed.borrow_mut().place(display, through, Instant::now())
            }),
            lift: Box::new(move |display, through| {
                if lifted.borrow_mut().lift(display, through, Instant::now()) {
                    wake.send(Event::Lifted);
                }
            }),
        };
        (RestActor { rx, borders }, handover)
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
        assert!(behaviour.contains(
            NSWindowCollectionBehavior::MoveToActiveSpace | NSWindowCollectionBehavior::Transient
        ));
        assert!(!behaviour.intersects(
            NSWindowCollectionBehavior::Stationary | NSWindowCollectionBehavior::Managed
        ));
    }
}
