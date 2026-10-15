//! A decoration at rest, drawn by a window of rini's own directly above the window it decorates:
//! which windows show a band and when, what each turn of the rest windows makes of the reactor's
//! word and the window server's, which windows are asked of the window server as they are first
//! named, what waits from a flight's begin for its place ahead of the lift, how many rest windows
//! rini keeps, by the fullest workspace on each display, which of them draws each and what each
//! changes to draw it, which are ordered above their windows again and when one takes its window's
//! level, and that none is kept and nothing taken while they are off. See "At rest" in `src/decorations/docs/README.md`.

use std::time::{Duration, Instant};

use rini_core::ids::{SpaceId, WindowServerId};
use rini_geometry::space::{Respace, ScreenRect, ScreenVector, TileSize};
use rustc_hash::FxHashMap as HashMap;

use crate::decorations::domain::shape;
use crate::decorations::domain::style::{Decoration, Ring, Style};
use crate::displays::event::{OrderChange, ServerOrder, StackLevel};

/// How long a change of `parked` waits for its window to move before it takes effect anyway.
pub const PARK_PATIENCE: Duration = Duration::from_millis(250);

/// How long a flight that has not shown holds the rest windows before they let go and draw what
/// waited: past the engine's wait for frame zero and the slowest show measured ("The handover" in
/// `src/decorations/docs/README.md`).
pub const FLIGHT_PATIENCE: Duration = Duration::from_millis(250);

/// Why a window shows no band wherever it is, as rini's own stores have it. A window on a Space its
/// display is not showing is not sent at all.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Held {
    /// rini has it off screen: its workspace is not the one its display shows, or its strip has it
    /// off the viewport. Read from rini's layout, never from the window's frame, of which macOS
    /// leaves up to 52pt on screen.
    pub parked: bool,
    pub mission_control: bool,
    /// In native fullscreen, or entering or leaving it.
    pub fullscreen: bool,
}

/// The workspace a window is on, as plain numbers: the Space it is on, which one display shows, and
/// the workspace's own, of which that display shows one at a time.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Group {
    pub space: SpaceId,
    pub workspace: u64,
}

/// One window that wears a decoration rini draws at rest, as the reactor sends it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RestWindow {
    pub window: WindowServerId,
    pub group: Group,
    pub decoration: Decoration,
    /// The managed window macOS has focused.
    pub focused: bool,
    pub ring: Ring,
    pub held: Held,
}

impl RestWindow {
    /// How its bands are painted: those it wears in their paints, the others none.
    pub fn style(&self) -> Style { Style::of(self.focused, self.ring, &self.decoration) }

    /// Whether it wears a band now: the ring, or a border.
    pub fn wears_a_band(&self) -> bool {
        let style = self.style();
        style.ring.is_some() || style.border.is_some()
    }

    /// How far its bands reach outside it, in points of a display of `scale`.
    pub fn outset(&self, scale: f64) -> f64 { shape::outset(&self.decoration, self.ring.worn(), scale) }
}

/// A display a band at rest is drawn on: its whole bounds and its scale.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RestDisplay {
    pub bounds: ScreenRect,
    pub scale: f64,
}

/// What the reactor sends the rest windows: the displays, and every window that wears something.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RestModel {
    pub displays: Vec<RestDisplay>,
    pub windows: Vec<RestWindow>,
}

/// The display a window at `frame` is drawn on: the one holding most of it.
pub fn display_of(frame: ScreenRect, displays: &[RestDisplay]) -> Option<RestDisplay> {
    let area = |display: &RestDisplay| frame.intersection(&display.bounds).map_or(0.0, |cut| cut.area());
    displays.iter().filter(|display| area(display) > 0.0).max_by(|a, b| area(a).total_cmp(&area(b))).copied()
}

/// Whether `window`'s bands show with its window at `frame`: it wears one, nothing holds it
/// (`parked` as it has taken effect), the window server has it ordered in, and some of the band is on
/// a display. Shown or not, a rest window stays ordered in: showing is its alpha alone.
pub fn shown(
    window: &RestWindow,
    parked: bool,
    ordered_out: bool,
    frame: Option<ScreenRect>,
    displays: &[RestDisplay],
) -> bool {
    let Held { parked: _, mission_control, fullscreen } = window.held;
    let on_a_display = frame.and_then(|frame| display_of(frame, displays)).is_some();
    window.wears_a_band() && !(parked || mission_control || fullscreen || ordered_out) && on_a_display
}

/// Whether a window is ordered out after `orders`, ordered out before them if `was`: its last hide
/// or unhide among them decides, and a close orders it out for good.
pub fn ordered_out(was: bool, window: WindowServerId, orders: &[ServerOrder]) -> bool {
    orders.iter().filter(|order| order.window == window).fold(was, |out, order| match order.change {
        OrderChange::Hidden | OrderChange::Closed => true,
        OrderChange::Unhidden => false,
        OrderChange::Reordered => out,
    })
}

/// A window's `parked` as its band takes it. rini parks a window and brings one back by moving it,
/// under the overlay when a flight runs, so a change waits for the move: it takes effect at the
/// window's next frame, at a lift on its display, or `PARK_PATIENCE` after it came, whichever is
/// first.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Parking {
    pub parked: bool,
    waiting: Option<(bool, Instant)>,
}

impl Parking {
    pub fn new(parked: bool) -> Parking { Parking { parked, waiting: None } }

    /// The reactor's word, at `at`.
    pub fn want(&mut self, parked: bool, at: Instant) {
        self.waiting = match self.waiting {
            _ if parked == self.parked => None,
            Some((wanted, since)) if wanted == parked => Some((wanted, since)),
            _ => Some((parked, at)),
        };
    }

    /// Takes a waiting change once its window `moved`, its display `lifted`, or it is due at `now`.
    pub fn settle(&mut self, moved: bool, lifted: bool, now: Instant) {
        if let Some((wanted, since)) = self.waiting
            && (moved || lifted || now >= since + PARK_PATIENCE)
        {
            self.parked = wanted;
            self.waiting = None;
        }
    }

    /// When a waiting change takes effect if nothing moves.
    pub fn due(&self) -> Option<Instant> { self.waiting.map(|(_, since)| since + PARK_PATIENCE) }
}

/// A window as the window server has it when first asked, off the main thread: its frame, whether
/// it is ordered out, its corner radius and its level and sub-level.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Introduction {
    pub frame: Option<ScreenRect>,
    pub ordered_out: bool,
    pub radius: Option<f64>,
    pub level: StackLevel,
}

/// What is known of each decorated window between the reactor's words. `radius` is the corner the
/// window server reports, which a window rule's goes ahead of, and `level` its place in the stack as
/// last read, at its introduction or its last reorder or unhide.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tracked {
    pub frame: Option<ScreenRect>,
    pub parking: Parking,
    pub ordered_out: bool,
    pub radius: Option<f64>,
    pub level: StackLevel,
}

/// The level and sub-level a rest window that `took` one takes to go above a window at `level`:
/// none when it holds that one already, since each take is two window-server calls.
pub fn stack_change(took: Option<StackLevel>, level: StackLevel) -> Option<StackLevel> {
    (took != Some(level)).then_some(level)
}

/// What a turn draws: each rest window's look where it changed (`delta`), and those whose window
/// the window server reordered or ordered back in (`orders`, `to_reorder`) and the one whose window
/// focus came to ordered above their windows again.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Draw {
    pub orders: Vec<ServerOrder>,
    pub focus: Option<WindowServerId>,
}

/// What a place draws, and the windows whose frame is read again from the window server first.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Placed {
    pub draw: Draw,
    pub reread: Vec<WindowServerId>,
}

/// A flight the reactor handed the engine: the display its overlay covers, the press it answers, and
/// when the rest windows heard of it.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Begun {
    display: ScreenRect,
    pressed: Instant,
    at: Instant,
}

/// What waits for the lift from a flight's begin over `display`: the reactor's latest word and the
/// window server's orders. `until` is when a flight whose overlay has not shown lets go; none once
/// it shows, when only its lift does. `begun` is the latest flight begun, so a lift that the engine
/// gave before that flight's press came hands the cover on to it.
#[derive(Clone, Debug, Default, PartialEq)]
struct Covered {
    display: Option<ScreenRect>,
    until: Option<Instant>,
    model: Option<RestModel>,
    orders: Vec<ServerOrder>,
    begun: Option<Begun>,
}

/// What the rest windows draw from: the reactor's latest model, and what is known of each window it
/// names. A window is known once the window server's introduction of it comes back (`ask`,
/// `introduce`), and is drawn from then. Each turn takes what queued: the model, the window server's
/// introductions, frames and orders, a lift.
/// From a flight's begin to its lift a turn draws nothing (`fly`, `cover`) unless a window on
/// another display was introduced, moved, was reordered or settled its park: the rest windows on the
/// flight's display are under its overlay or show what was there before the press, as its frame
/// zero does. Once the flight has landed, a tick before its lift or with it, the place draws what
/// came, still covered (`place`), and the lift what came since (`lift`). A place or a lift the
/// engine gave before the press of a flight begun since leaves what waited for that flight, and the
/// lift hands it the cover, as a flight beginning with no overlay up. A flight whose overlay
/// never shows lets go `FLIGHT_PATIENCE` after it began (`settle`). Off, as it starts and once the reactor turns it
/// off, it holds no model and no window, wants no rest window, and a flight, a turn, a place, a lift
/// and a park's patience draw nothing; the reactor's next model turns it on.
#[derive(Clone, Debug, Default)]
pub struct Rest {
    pub model: RestModel,
    pub windows: HashMap<WindowServerId, Tracked>,
    /// Each window asked of the window server and not yet known: none until its introduction comes
    /// back, then it, with each frame and order since, until the model naming it is taken.
    introducing: HashMap<WindowServerId, Option<Introduction>>,
    covered: Option<Covered>,
    on: bool,
}

impl Rest {
    /// Whether a model has come since it started or was last turned off.
    pub fn on(&self) -> bool { self.on }

    /// No window can wear a band: the model, every window known or asked and every word and order
    /// waiting for the lift are dropped, and nothing is taken until the next model. Whether a flight or the
    /// overlay covers them is kept, so a model that turns it on under the overlay waits for the lift.
    pub fn off(&mut self) {
        self.on = false;
        self.model = RestModel::default();
        self.windows.clear();
        self.introducing.clear();
        if let Some(covered) = self.covered.as_mut() {
            *covered = Covered { display: covered.display, until: covered.until, begun: covered.begun, ..Covered::default() };
        }
    }

    /// A flight over `display` for the press at `pressed` began at `now`: until its lift, or
    /// `FLIGHT_PATIENCE` if its overlay has not shown by then, a turn draws nothing unless a window on
    /// another display moved. A flight beginning under an overlay already up is that overlay's to
    /// lift, unless the engine gave that lift before its press (`lift`). Off, it takes nothing.
    pub fn fly(&mut self, display: ScreenRect, pressed: Instant, now: Instant) {
        if !self.on {
            return;
        }
        let up = self.covered.as_ref().is_some_and(|covered| covered.until.is_none());
        let covered = self.covered.get_or_insert_with(Covered::default);
        covered.begun = Some(Begun { display, pressed, at: now });
        if !up {
            covered.display = Some(display);
            covered.until = Some(now + FLIGHT_PATIENCE);
        }
    }

    /// The flight begun after the press a place or a lift the engine gave is `through`: the engine
    /// had given it before that press came, so that flight is not the one landing.
    fn begun_after(&self, through: Option<Instant>) -> Option<Begun> {
        self.covered.as_ref()?.begun.filter(|begun| Some(begun.pressed) > through)
    }

    /// The overlay is up over `display`, the one it was laid out on: until it lifts, a turn draws
    /// nothing unless a window on another display moved.
    pub fn cover(&mut self, display: Option<ScreenRect>) {
        let covered = self.covered.get_or_insert_with(Covered::default);
        covered.display = display;
        covered.until = None;
    }

    #[cfg(test)]
    pub fn covered(&self) -> bool { self.covered.is_some() }

    /// One turn at `now`: the reactor's word if one came, the window server's introductions, its
    /// frames, each latest last, and its orders. Uncovered, the turn draws them. Covered, the
    /// introductions, the frames and the orders are taken, so a park is taken with its window's move
    /// and a hide from its order, but the word and the orders' reordering wait for the lift, and
    /// nothing is drawn. An introduction, a frame or an order of a window on a display the overlay
    /// is not over draws the turn as uncovered, with everything that waited. Off, a turn without a
    /// model takes nothing and draws nothing.
    pub fn turn(
        &mut self,
        mut model: Option<RestModel>,
        intros: &[(WindowServerId, Introduction)],
        frames: &[(WindowServerId, ScreenRect)],
        mut orders: Vec<ServerOrder>,
        now: Instant,
    ) -> Option<Draw> {
        if !self.on && model.is_none() {
            return None;
        }
        let displays = &self.model.displays;
        let tracked = |window: WindowServerId| self.windows.get(&window).and_then(|tracked| tracked.frame);
        let seen = self.covered.as_ref().is_some_and(|covered| {
            let seen = |frame| elsewhere(frame, displays, covered.display);
            intros.iter().any(|(_, intro)| seen(intro.frame))
                || frames.iter().any(|(window, frame)| seen(Some(*frame)) || seen(tracked(*window)))
                || orders.iter().any(|order| seen(tracked(order.window)))
        });
        if seen && let Some(covered) = self.covered.as_mut() {
            model = model.or(covered.model.take());
            orders.splice(0..0, std::mem::take(&mut covered.orders));
        }
        let focus = match &mut self.covered {
            Some(covered) if !seen => {
                covered.model = model.or(covered.model.take());
                covered.orders.extend(&orders);
                None
            }
            _ => Some(model.and_then(|model| self.take_model(model, now))),
        };
        self.introduce(intros);
        self.take_frames(frames, now);
        self.take_orders(&orders);
        focus.map(|focus| Draw { orders, focus })
    }

    /// The windows `model` names that are neither known nor asked yet, each now asked: the
    /// platform has the window server read each off the main thread and hands back its
    /// introduction. Asked as a model arrives, covered or not, so one first named under the overlay
    /// is known by its lift.
    pub fn ask(&mut self, model: &RestModel) -> Vec<WindowServerId> {
        let mut asked = Vec::new();
        for window in &model.windows {
            if !self.windows.contains_key(&window.window) && !self.introducing.contains_key(&window.window) {
                self.introducing.insert(window.window, None);
                asked.push(window.window);
            }
        }
        asked
    }

    /// The window server's introductions of windows asked: each the model names is known from now,
    /// and one whose model waits under the overlay is kept for it. One not asked, or no longer
    /// named by the model taken since it was asked, is dropped.
    pub fn introduce(&mut self, intros: &[(WindowServerId, Introduction)]) {
        for (window, intro) in intros {
            if let Some(waiting) = self.introducing.get_mut(window) {
                *waiting = Some(*intro);
            }
        }
        self.adopt();
    }

    /// Each window the model names whose introduction came, known from now.
    fn adopt(&mut self) {
        for window in &self.model.windows {
            let came = self.introducing.get(&window.window).copied().flatten();
            let Some(intro) = came.filter(|_| !self.windows.contains_key(&window.window)) else { continue };
            self.introducing.remove(&window.window);
            let Introduction { frame, ordered_out, radius, level } = intro;
            let parking = Parking::new(window.held.parked);
            self.windows.insert(window.window, Tracked { frame, parking, ordered_out, radius, level });
        }
    }

    /// The reactor's word at `now`: a window it no longer names is forgotten, one whose introduction
    /// came is known, each park waits for its window's move, and the window focus came to is named,
    /// when it changed, so its rest window is ordered again.
    pub fn take_model(&mut self, model: RestModel, now: Instant) -> Option<WindowServerId> {
        let focus_before = self.focus();
        self.on = true;
        let named = |window: &WindowServerId| model.windows.iter().any(|w| w.window == *window);
        self.windows.retain(|window, _| named(window));
        self.introducing.retain(|window, _| named(window));
        for window in &model.windows {
            if let Some(tracked) = self.windows.get_mut(&window.window) {
                tracked.parking.want(window.held.parked, now);
            }
        }
        self.model = model;
        self.adopt();
        self.focus().filter(|focus| Some(*focus) != focus_before)
    }

    fn focus(&self) -> Option<WindowServerId> {
        self.model.windows.iter().find(|window| window.focused).map(|window| window.window)
    }

    /// Where the window server has windows that moved, each latest last: a waiting park is taken
    /// with its window's move, and an introduction waiting for its model takes the frame.
    pub fn take_frames(&mut self, frames: &[(WindowServerId, ScreenRect)], now: Instant) {
        for (window, frame) in frames {
            if let Some(tracked) = self.windows.get_mut(window) {
                tracked.frame = Some(*frame);
                tracked.parking.settle(true, false, now);
            } else if let Some(Some(intro)) = self.introducing.get_mut(window) {
                intro.frame = Some(*frame);
            }
        }
    }

    /// The window server's orders, in the order they came: whether each window is ordered out, and
    /// its level as read after its last reorder or unhide, known or introduced and waiting for its
    /// model.
    pub fn take_orders(&mut self, orders: &[ServerOrder]) {
        for (window, tracked) in self.windows.iter_mut() {
            tracked.ordered_out = ordered_out(tracked.ordered_out, *window, orders);
        }
        for (window, intro) in self.introducing.iter_mut().filter_map(|(window, intro)| Some((window, intro.as_mut()?))) {
            intro.ordered_out = ordered_out(intro.ordered_out, *window, orders);
        }
        for order in orders {
            let Some(level) = order.level else { continue };
            if let Some(tracked) = self.windows.get_mut(&order.window) {
                tracked.level = level;
            } else if let Some(Some(intro)) = self.introducing.get_mut(&order.window) {
                intro.level = level;
            }
        }
    }

    /// The flight over `display`, the one the overlay was laid out on if any, through the press at
    /// `through`, has landed at `now`, a tick before its lift or with it: what waited is taken, the
    /// reactor's word with `take_model`, and every park waiting on a window there; the word and the
    /// orders wait on for a flight begun after `through`. It draws what waited, still covered,
    /// after reading again from the window server the frame of each window there brought back and
    /// each one there a rest window draws (`drawn`). Off, with no model waiting, it draws nothing and
    /// reads nothing.
    pub fn place(
        &mut self,
        display: Option<ScreenRect>,
        through: Option<Instant>,
        now: Instant,
        drawn: impl Fn(WindowServerId) -> bool,
    ) -> Placed {
        let next = self.begun_after(through);
        let (model, orders) = self
            .covered
            .as_mut()
            .filter(|_| next.is_none())
            .map(|covered| (covered.model.take(), std::mem::take(&mut covered.orders)))
            .unwrap_or_default();
        let focus = model.and_then(|model| self.take_model(model, now));
        if !self.on {
            return Placed::default();
        }
        let landed = self.land(display, now).into_iter();
        let reread = landed.filter(|(window, back)| *back || drawn(*window)).map(|(window, _)| window).collect();
        Placed { draw: Draw { orders, focus }, reread }
    }

    /// The overlay of the flight through the press at `through` lifts at `now` off `display`: what
    /// came since the place is taken, and every park on a window there that waited, and it draws
    /// what came, reading nothing, as the place before it read what it draws and a covered turn takes
    /// each frame since. A flight begun after `through` takes the cover on, with the word and the
    /// orders that waited, until its own lift or its patience from its begin. Off, with no model
    /// waiting, it draws nothing.
    pub fn lift(&mut self, display: Option<ScreenRect>, through: Option<Instant>, now: Instant) -> Draw {
        let next = self.begun_after(through);
        let Covered { model, orders, .. } = self.covered.take().unwrap_or_default();
        if let Some(begun) = next {
            let until = Some(begun.at + FLIGHT_PATIENCE);
            self.covered = Some(Covered { display: Some(begun.display), until, model, orders, begun: next });
            if self.on {
                self.land(display, now);
            }
            return Draw::default();
        }
        let focus = model.and_then(|model| self.take_model(model, now));
        if !self.on {
            return Draw::default();
        }
        self.land(display, now);
        Draw { orders, focus }
    }

    /// Every park waiting on a window on `display` taken at `now`: each window there, and each one a
    /// park brought back, with whether it was brought back.
    fn land(&mut self, display: Option<ScreenRect>, now: Instant) -> Vec<(WindowServerId, bool)> {
        let mut landed = Vec::new();
        for (window, tracked) in self.windows.iter_mut() {
            let there = display.is_some_and(|display| tracked.frame.is_some_and(|frame| frame.intersects(&display)));
            let parked = tracked.parking.parked;
            tracked.parking.settle(false, there, now);
            let brought_back = parked && !tracked.parking.parked;
            if brought_back || there {
                landed.push((*window, brought_back));
            }
        }
        landed
    }

    /// `window`'s frame as the window server has it now, kept as it was if it has none.
    pub fn reread(&mut self, window: WindowServerId, frame: Option<ScreenRect>) {
        if let Some(tracked) = self.windows.get_mut(&window) {
            tracked.frame = frame.or(tracked.frame);
        }
    }

    /// What ran out of patience at `now`, taken: a flight that has not shown lets go, with the
    /// reactor's word that waited taken with `take_model` and the orders that waited, and each park
    /// whose patience ran out. What the turn draws, which it does unless something still covers it
    /// and every park taken is on its display; none while off.
    pub fn settle(&mut self, now: Instant) -> Option<Draw> {
        let spent = self.covered.as_ref().and_then(|covered| covered.until).is_some_and(|until| now >= until);
        let let_go = spent.then(|| self.covered.take()).flatten().map(|Covered { model, orders, .. }| {
            let focus = model.and_then(|model| self.take_model(model, now));
            Draw { orders, focus }
        });
        if !self.on {
            return None;
        }
        let over = self.covered.as_ref().map(|covered| covered.display);
        let mut seen = false;
        for tracked in self.windows.values_mut() {
            let before = tracked.parking;
            tracked.parking.settle(false, false, now);
            seen |= tracked.parking != before && elsewhere(tracked.frame, &self.model.displays, over.flatten());
        }
        let_go.or_else(|| (over.is_none() || seen).then(Draw::default))
    }

    /// When the next waiting park takes effect, or a flight that has not shown lets go.
    pub fn due(&self) -> Option<Instant> {
        let flight = self.covered.as_ref().and_then(|covered| covered.until);
        self.windows.values().filter_map(|tracked| tracked.parking.due()).chain(flight).min()
    }

    /// Whether, after a lift, the rest windows want a turn of their own with `pool` rest windows: the
    /// pool is short, or a park the lift took no move for waits for its patience, whose timer only
    /// such a turn sets.
    pub fn wants_turn(&self, pool: usize) -> bool { pool < self.pool_size() || self.due().is_some() }

    /// How many rest windows the pool holds for this model: for each display, the most windows one of
    /// its workspaces has that wear a band and could show it, held by no more than a park, and one
    /// spare. A display shows one workspace at a time, and a flight's place takes the parks on its
    /// display before it draws, so a flown switch wants only the arriving workspace's; the spare lets
    /// the ring moving with focus, and the leaving and arriving ring of a switch no flight covers,
    /// show together. With the border on such a switch can be short until the leaving parks take
    /// effect. None at all while off.
    pub fn pool_size(&self) -> usize {
        if !self.on {
            return 0;
        }
        let mut bands: HashMap<Group, usize> = HashMap::default();
        for window in self.model.windows.iter().filter(|window| {
            let Held { parked: _, mission_control, fullscreen } = window.held;
            window.wears_a_band() && !(mission_control || fullscreen)
        }) {
            *bands.entry(window.group).or_default() += 1;
        }
        let mut most: HashMap<SpaceId, usize> = HashMap::default();
        for (group, count) in bands {
            let space = most.entry(group.space).or_default();
            *space = (*space).max(count);
        }
        most.values().sum::<usize>() + self.model.displays.len()
    }

    /// Every window whose bands show now, in the model's order.
    pub fn wanted(&self) -> Vec<WindowServerId> {
        let displays = &self.model.displays;
        self.model
            .windows
            .iter()
            .filter(|window| {
                self.windows.get(&window.window).is_some_and(|tracked| {
                    shown(window, tracked.parking.parked, tracked.ordered_out, tracked.frame, displays)
                })
            })
            .map(|window| window.window)
            .collect()
    }
}

/// Whether a window at `frame` is on a display, and not the one the overlay is over (`over`).
fn elsewhere(frame: Option<ScreenRect>, displays: &[RestDisplay], over: Option<ScreenRect>) -> bool {
    frame
        .and_then(|frame| display_of(frame, displays))
        .is_some_and(|display| !over.is_some_and(|over| display.bounds.intersects(&over)))
}

/// Which rest window draws each window whose band shows.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Assignment {
    /// Rest windows given a window, by their place in the pool: each is ordered above it.
    pub retargeted: Vec<(usize, WindowServerId)>,
    /// Rest windows whose window shows nothing now: drawn at alpha 0, kept for the next.
    pub released: Vec<usize>,
}

/// One rest window for each of `wanted`, from `pool`, each window's position the window it draws if
/// any, and `drew` the window whose look each one last drew: a rest window already on a wanted window
/// keeps it, and the others, first in `wanted`'s order, are given one while the pool has one left
/// (`Rest::pool_size`). A free rest window goes back to the window it last drew, so a band shown
/// again where it was re-lays and moves nothing; a window none drew takes one free before the turn,
/// one that drew nothing first, and one freed this turn last, which keeps what it drew the longest.
pub fn assign(pool: &[Option<WindowServerId>], drew: &[Option<WindowServerId>], wanted: &[WindowServerId]) -> Assignment {
    let kept = |target: &Option<WindowServerId>| target.is_some_and(|target| wanted.contains(&target));
    let mut free: Vec<usize> = (0..pool.len()).filter(|at| !kept(&pool[*at])).collect();
    let needing: Vec<WindowServerId> = wanted.iter().copied().filter(|window| !pool.contains(&Some(*window))).collect();
    let given = &needing[..needing.len().min(free.len())];
    let mut assignment = Assignment::default();
    let mut left = Vec::new();
    for window in given {
        match free.iter().position(|at| drew.get(*at).copied().flatten() == Some(*window)) {
            Some(i) => assignment.retargeted.push((free.remove(i), *window)),
            None => left.push(*window),
        }
    }
    free.sort_by_key(|at| (pool[*at].is_some(), drew.get(*at).copied().flatten().is_some()));
    let mut free = free.into_iter();
    for window in left {
        let Some(at) = free.next() else { break };
        assignment.retargeted.push((at, window));
    }
    assignment.retargeted.sort_by_key(|(_, window)| wanted.iter().position(|w| w == window));
    assignment.released = free.filter(|at| pool[*at].is_some()).collect();
    assignment.released.sort_unstable();
    assignment
}

/// The rest windows that show a window afresh this turn: each one of `pool`, its window and whether
/// it was shown before the turn, whose window shows now (`wanted`) and that was `given` it this turn
/// or was not shown. Each goes to alpha 0 until its new layers are committed and only then takes its
/// frame and alpha 1, so it never shows one window's band in another's place.
pub fn afresh(pool: &[(Option<WindowServerId>, bool)], given: &[usize], wanted: &[WindowServerId]) -> Vec<usize> {
    let shows = |target: &Option<WindowServerId>| target.is_some_and(|target| wanted.contains(&target));
    pool.iter()
        .enumerate()
        .filter(|(at, (target, shown))| shows(target) && (given.contains(at) || !shown))
        .map(|(at, _)| at)
        .collect()
}

/// What a rest window draws: its bands' layers, its frame on screen, and whether it shows. Each keeps
/// the look it last drew, and a turn changes only what differs from it (`delta`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Look {
    pub layers: Layers,
    /// The band's outer rect cut to its display (`shape::rest_frame`).
    pub outer: ScreenRect,
    pub shown: bool,
}

/// What a rest window's layers are laid from: what its window wears and in which paints, the
/// window's size and corner, the display's scale, and where the window sits in the rest window.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Layers {
    pub decoration: Decoration,
    pub style: Style,
    pub size: TileSize,
    pub corner: f64,
    pub scale: f64,
    pub inside: ScreenVector,
}

impl Look {
    /// `window`'s bands, shown, around its window at `frame` on `display`, its corner `radius` as the
    /// window server reports it and a window rule's ahead of it; none when no band of it is on the
    /// display.
    pub fn of(window: &RestWindow, frame: ScreenRect, radius: Option<f64>, display: RestDisplay) -> Option<Look> {
        let outer = shape::rest_frame(frame, window.outset(display.scale), display.bounds)?;
        let size = frame.size.respace();
        let corner = shape::radius(window.decoration.corner(|| radius), size);
        let layers = Layers {
            decoration: window.decoration,
            style: window.style(),
            size,
            corner,
            scale: display.scale,
            inside: frame.origin - outer.origin,
        };
        Some(Look { layers, outer, shown: true })
    }

    /// The same, at alpha 0: its layers and its frame stay as they were.
    pub fn hidden(self) -> Look { Look { shown: false, ..self } }
}

/// What a rest window changes to draw a look: its layers laid again, its frame, its alpha.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Delta {
    pub layers: bool,
    pub frame: bool,
    pub alpha: bool,
}

/// What a rest window that drew `was`, none since it was made at alpha 0, changes to draw `now`.
/// One going to alpha 0 changes its alpha alone, so a window shown again keeps the layers and frame
/// it hid with and lays only what changed meanwhile.
pub fn delta(was: Option<Look>, now: Look) -> Delta {
    Delta {
        layers: now.shown && was.is_none_or(|was| was.layers != now.layers),
        frame: now.shown && was.is_none_or(|was| was.outer != now.outer),
        alpha: was.is_some_and(|was| was.shown) != now.shown,
    }
}

/// The rest windows to order above their windows again this turn, each once: those whose window the
/// window server reordered or ordered back in, and those in `also`, a rest window just given a window
/// or the one whose window focus came to. `targets` is the pool, each one's window if any.
pub fn to_reorder(orders: &[ServerOrder], targets: &[Option<WindowServerId>], also: &[usize]) -> Vec<usize> {
    let moved = orders
        .iter()
        .filter(|order| matches!(order.change, OrderChange::Reordered | OrderChange::Unhidden))
        .filter_map(|order| targets.iter().position(|target| *target == Some(order.window)));
    let mut reorder: Vec<usize> = Vec::new();
    for at in also.iter().copied().chain(moved) {
        if !reorder.contains(&at) {
            reorder.push(at);
        }
    }
    reorder
}

#[cfg(test)]
mod tests {
    use rini_geometry::space::{Point, Size};

    use super::*;
    use crate::decorations::domain::paint::{Colour, Paint};
    use crate::decorations::domain::style::Band;

    const BAND: Band = Band { width: 4.0, active: Paint::Solid(Colour::rgb(0x7fc8ff)), inactive: Paint::Solid(Colour::rgb(0x505050)), urgent: Paint::Solid(Colour::rgb(0x9b0000)) };
    const BORDER: Band = Band { width: 4.0, active: Paint::Solid(Colour::rgb(0xffc87f)), inactive: Paint::Solid(Colour::rgb(0x505050)), urgent: Paint::Solid(Colour::rgb(0x9b0000)) };

    fn ws(id: u32) -> WindowServerId { WindowServerId::new(id) }

    fn rect(x: f64, y: f64, w: f64, h: f64) -> ScreenRect { ScreenRect::new(Point::new(x, y), Size::new(w, h)) }

    fn display() -> Vec<RestDisplay> { vec![RestDisplay { bounds: rect(0.0, 0.0, 1728.0, 1117.0), scale: 2.0 }] }

    fn group(workspace: u64) -> Group { Group { space: SpaceId::new(1), workspace } }

    fn ringed(held: Held) -> RestWindow {
        RestWindow {
            window: ws(56),
            group: group(1),
            decoration: Decoration { ring: Some(BAND), ..Decoration::default() },
            focused: true,
            ring: Ring::Active,
            held,
        }
    }

    const SLOT: ScreenRect = ScreenRect::new(Point::new(3.0, 35.0), Size::new(859.0, 1079.0));

    #[test]
    fn a_ringed_window_on_its_display_shows_its_band() {
        assert!(shown(&ringed(Held::default()), false, false, Some(SLOT), &display()));
        let unringed = RestWindow { ring: Ring::None, ..ringed(Held::default()) };
        assert!(!shown(&unringed, false, false, Some(SLOT), &display()), "it wears nothing");
        assert!(!shown(&ringed(Held::default()), false, false, None, &display()), "no frame yet");
    }

    #[test]
    fn a_bordered_window_shows_its_band_whether_it_wears_the_ring_or_not() {
        let bordered = |ring| RestWindow {
            decoration: Decoration { border: Some(BORDER), ..ringed_window().decoration },
            ring,
            focused: false,
            ..ringed_window()
        };
        for ring in [Ring::Inactive, Ring::None] {
            assert!(shown(&bordered(ring), false, false, Some(SLOT), &display()), "{ring:?}");
        }
        assert_eq!(bordered(Ring::None).style().border, Some(BORDER.inactive));
        assert_eq!((bordered(Ring::Inactive).outset(2.0), bordered(Ring::None).outset(2.0)), (8.0, 4.0), "the ring outside the border");
    }

    fn ringed_window() -> RestWindow { ringed(Held::default()) }

    /// macOS clamps a parked Chrome or Finder window so 52pt of it shows, past the 40pt
    /// `is_off_screen` allows: only rini's word says it is parked.
    #[test]
    fn a_parked_window_shows_no_band_with_its_sliver_on_screen() {
        let sliver = rect(1676.0, 35.0, 1148.0, 1079.0);
        let window = ringed(Held { parked: true, ..Held::default() });
        assert!(!shown(&window, true, false, Some(sliver), &display()));
        assert!(shown(&window, false, false, Some(SLOT), &display()), "until the park takes effect");
    }

    #[test]
    fn no_band_shows_while_mission_control_is_up() {
        let window = ringed(Held { mission_control: true, ..Held::default() });
        assert!(!shown(&window, false, false, Some(SLOT), &display()));
    }

    #[test]
    fn a_window_in_native_fullscreen_shows_no_band() {
        let window = ringed(Held { fullscreen: true, ..Held::default() });
        assert!(!shown(&window, false, false, Some(SLOT), &display()));
    }

    /// Minimised, its app hidden, Cmd-H: the window server orders it out, and the band goes with it
    /// until the window is ordered back in.
    #[test]
    fn a_window_the_window_server_orders_out_shows_no_band_until_it_is_unhidden() {
        let order = |change| ServerOrder { window: ws(56), change, level: None };
        let other = ServerOrder { window: ws(57), change: OrderChange::Unhidden, level: None };
        let out = ordered_out(false, ws(56), &[order(OrderChange::Reordered), order(OrderChange::Hidden), other]);
        assert!(out);
        assert!(!shown(&ringed(Held::default()), false, out, Some(SLOT), &display()));
        assert!(ordered_out(true, ws(56), &[order(OrderChange::Reordered)]), "a reorder unhides nothing");
        assert!(!ordered_out(true, ws(56), &[order(OrderChange::Unhidden)]));
    }

    #[test]
    fn a_park_waits_for_its_window_to_move_the_lift_or_its_patience() {
        let at = Instant::now();
        let mut parking = Parking::new(false);
        parking.want(true, at);
        parking.settle(false, false, at + Duration::from_millis(10));
        assert!(!parking.parked, "the window has not moved yet");
        assert_eq!(parking.due(), Some(at + PARK_PATIENCE));
        parking.settle(true, false, at + Duration::from_millis(10));
        assert!(parking.parked, "it moved");
        parking.want(false, at);
        parking.settle(false, true, at);
        assert!(!parking.parked, "the lift");
        parking.want(true, at);
        parking.settle(false, false, at + PARK_PATIENCE);
        assert!(parking.parked, "patience spent");
        parking.want(false, at);
        parking.want(true, at);
        assert_eq!(parking.due(), None, "wanted back as it is: nothing waits");
    }

    fn model(windows: Vec<RestWindow>) -> RestModel { RestModel { displays: display(), windows } }

    /// Window `id`, ringed, the active window of a workspace of its own on the first display.
    fn at(id: u32, focused: bool, parked: bool) -> RestWindow {
        RestWindow { window: ws(id), group: group(id.into()), focused, held: Held { parked, ..Held::default() }, ..ringed_window() }
    }

    const LEVEL: StackLevel = StackLevel { level: Some(0), sub_level: Some(0) };

    fn intro(frame: ScreenRect) -> Introduction { Introduction { frame: Some(frame), ordered_out: false, radius: None, level: LEVEL } }

    /// `model` taken, each window it names new to `rest` asked and introduced at the frame `frames`
    /// gives it, as its introduction comes back after the model.
    fn take_known(rest: &mut Rest, model: RestModel, now: Instant, frames: impl Fn(WindowServerId) -> ScreenRect) -> Option<WindowServerId> {
        let asked = rest.ask(&model);
        let focus = rest.take_model(model, now);
        rest.introduce(&asked.into_iter().map(|window| (window, intro(frames(window)))).collect::<Vec<_>>());
        focus
    }

    fn rest_with(windows: Vec<RestWindow>, frame: ScreenRect, now: Instant) -> Rest {
        let mut rest = Rest::default();
        take_known(&mut rest, model(windows), now, |_| frame);
        rest
    }

    #[test]
    fn the_window_focus_comes_to_is_named_and_a_model_with_focus_where_it_was_names_none() {
        let now = Instant::now();
        let mut rest = Rest::default();
        assert_eq!(take_known(&mut rest, model(vec![at(1, true, false), at(2, false, false)]), now, |_| SLOT), Some(ws(1)));
        assert_eq!(rest.take_model(model(vec![at(1, true, false), at(2, false, true)]), now), None, "focus unchanged");
        assert_eq!(rest.take_model(model(vec![at(1, false, false), at(2, true, false)]), now), Some(ws(2)));
        assert_eq!(rest.take_model(model(vec![at(1, false, false)]), now), None, "no window focused");
        assert_eq!(rest.windows.len(), 1, "a window the reactor no longer names is forgotten");
    }

    /// A window first named is asked of the window server once, read off the main thread, and drawn
    /// from when its introduction comes back; one the model let go of meanwhile is not.
    #[test]
    fn a_window_new_to_the_model_is_drawn_once_introduced() {
        let now = Instant::now();
        let mut rest = Rest::default();
        let first = model(vec![at(1, true, false), at(2, false, false)]);
        assert_eq!(rest.ask(&first), vec![ws(1), ws(2)]);
        assert_eq!(rest.ask(&first), Vec::new(), "asked once");
        let drawn = rest.turn(Some(first.clone()), &[], &[(ws(1), SLOT)], Vec::new(), now);
        assert_eq!(drawn, Some(Draw { orders: Vec::new(), focus: Some(ws(1)) }));
        assert!(rest.wanted().is_empty() && rest.windows.is_empty(), "not drawn before it is known");
        let level = StackLevel { level: Some(3), sub_level: Some(1) };
        let read = Introduction { frame: Some(SLOT), ordered_out: true, radius: Some(16.0), level };
        assert!(rest.turn(None, &[(ws(1), read), (ws(9), intro(SLOT))], &[], Vec::new(), now).is_some());
        assert_eq!(rest.windows[&ws(1)], Tracked { frame: Some(SLOT), parking: Parking::new(false), ordered_out: true, radius: Some(16.0), level });
        assert!(!rest.windows.contains_key(&ws(9)), "never asked");
        rest.turn(None, &[(ws(1), intro(SLOT))], &[], vec![ServerOrder { window: ws(1), change: OrderChange::Unhidden, level: None }], now);
        assert_eq!(rest.wanted(), vec![ws(1)], "known, ordered back in");
        assert_eq!(rest.windows[&ws(1)].radius, Some(16.0), "an introduction not asked again is dropped");
        rest.take_model(model(vec![at(1, true, false)]), now);
        rest.introduce(&[(ws(2), intro(SLOT))]);
        assert!(!rest.windows.contains_key(&ws(2)), "let go of before its introduction came");
        assert_eq!(rest.ask(&first), vec![ws(2)], "asked again once named again");
    }

    /// A rest window takes its window's level and sub-level as last read, at the introduction or
    /// after a reorder or an unhide, and takes it again only when it changed.
    #[test]
    fn a_rest_window_takes_its_windows_level_only_when_it_changed() {
        let now = Instant::now();
        let mut rest = rest_with(vec![at(1, true, false)], SLOT, now);
        assert_eq!(rest.windows[&ws(1)].level, LEVEL);
        let raised = StackLevel { level: Some(3), sub_level: Some(1) };
        let order = |change, level| ServerOrder { window: ws(1), change, level };
        rest.turn(None, &[], &[], vec![order(OrderChange::Reordered, Some(raised)), order(OrderChange::Hidden, None)], now);
        assert_eq!(rest.windows[&ws(1)].level, raised, "a hide reads no level");
        assert_eq!(stack_change(None, LEVEL), Some(LEVEL), "a rest window that took none");
        assert_eq!(stack_change(Some(LEVEL), LEVEL), None, "the one it took");
        assert_eq!(stack_change(Some(LEVEL), raised), Some(raised));
    }

    #[test]
    fn a_window_followed_through_a_drag_is_shown_throughout() {
        let now = Instant::now();
        let mut rest = rest_with(vec![at(1, true, false)], SLOT, now);
        for x in [3.0, 120.0, 900.0, 1500.0, -600.0] {
            rest.take_frames(&[(ws(1), rect(x, 35.0, 859.0, 1079.0))], now);
            assert_eq!(rest.wanted(), vec![ws(1)], "at {x}");
        }
        rest.take_frames(&[(ws(1), rect(500.0, 35.0, 859.0, 1079.0)), (ws(1), rect(1800.0, 35.0, 859.0, 1079.0))], now);
        assert_eq!(rest.windows[&ws(1)].frame, Some(rect(1800.0, 35.0, 859.0, 1079.0)), "the latest frame");
        assert!(rest.wanted().is_empty(), "dragged off every display");
    }

    #[test]
    fn a_park_is_taken_with_its_windows_move() {
        let now = Instant::now();
        let mut rest = rest_with(vec![at(1, true, false)], SLOT, now);
        rest.take_model(model(vec![at(1, true, true)]), now);
        assert_eq!(rest.wanted(), vec![ws(1)], "until its window moves");
        rest.take_frames(&[(ws(2), rect(1676.0, 35.0, 1148.0, 1079.0))], now);
        assert_eq!(rest.wanted(), vec![ws(1)], "another window's move");
        rest.take_frames(&[(ws(1), rect(1676.0, 35.0, 1148.0, 1079.0))], now);
        assert!(rest.wanted().is_empty());
    }

    #[test]
    fn a_window_the_window_server_hides_is_wanted_again_once_it_is_unhidden() {
        let now = Instant::now();
        let mut rest = rest_with(vec![at(1, true, false)], SLOT, now);
        rest.take_orders(&[ServerOrder { window: ws(1), change: OrderChange::Hidden, level: None }]);
        assert!(rest.wanted().is_empty());
        rest.take_orders(&[ServerOrder { window: ws(1), change: OrderChange::Unhidden, level: None }]);
        assert_eq!(rest.wanted(), vec![ws(1)]);
    }

    /// A window leaves the screen as it closes, before the reactor hears of it: its band goes then.
    #[test]
    fn a_window_the_window_server_closes_shows_no_band_from_its_close() {
        let now = Instant::now();
        let mut rest = rest_with(vec![at(1, true, false)], SLOT, now);
        rest.take_orders(&[ServerOrder { window: ws(1), change: OrderChange::Closed, level: None }]);
        assert!(rest.wanted().is_empty());
        let targets = [Some(ws(1))];
        assert_eq!(to_reorder(&[ServerOrder { window: ws(1), change: OrderChange::Closed, level: None }], &targets, &[]), Vec::<usize>::new());
    }

    /// A pass that changes two displays parks a window on each; a flight landing on one places it.
    #[test]
    fn a_place_takes_the_parks_on_its_display_alone_and_reads_again_what_it_draws_there() {
        let now = Instant::now();
        let other = rect(1728.0, 0.0, 1920.0, 1080.0);
        let mut rest = Rest::default();
        let frames = |window: WindowServerId| match window.as_u32() {
            1 | 3 => SLOT,
            _ => rect(1731.0, 35.0, 859.0, 1079.0),
        };
        take_known(&mut rest, model(vec![at(1, true, false), at(2, false, false), at(3, false, true)]), now, frames);
        rest.take_model(model(vec![at(1, true, true), at(2, false, true), at(3, false, false)]), now);
        let mut reread = rest.place(Some(display()[0].bounds), Some(now), now, |window| window == ws(1)).reread;
        reread.sort_by_key(|window| window.as_u32());
        assert_eq!(reread, vec![ws(1), ws(3)], "drawn there, and brought back there");
        assert!(rest.windows[&ws(1)].parking.parked && !rest.windows[&ws(3)].parking.parked);
        assert!(!rest.windows[&ws(2)].parking.parked, "a park on another display waits for its own move");
        assert_eq!(rest.place(Some(other), Some(now), now, |_| false).reread, Vec::new(), "parked, drawn by none");
        assert!(rest.windows[&ws(2)].parking.parked, "its own display's place");
        rest.reread(ws(1), None);
        assert_eq!(rest.windows[&ws(1)].frame, Some(SLOT), "no frame read keeps the one known");
    }

    /// An abandoned flight lifts with no place, and a model can come between the place and the
    /// lift: either way the lift takes each park waiting on its display.
    #[test]
    fn a_lift_takes_the_parks_waiting_on_its_display() {
        let now = Instant::now();
        for placed in [false, true] {
            let mut rest = rest_with(vec![at(1, true, false), at(2, false, false)], SLOT, now);
            rest.cover(Some(display()[0].bounds));
            if placed {
                rest.place(Some(display()[0].bounds), Some(now), now, |_| true);
            }
            assert_eq!(rest.turn(Some(model(vec![at(1, true, false), at(2, false, true)])), &[], &[], Vec::new(), now + ms(5)), None);
            assert!(!rest.windows[&ws(2)].parking.parked, "placed first: {placed}, under the cover with no move");
            rest.lift(Some(display()[0].bounds), Some(now + ms(10)), now + ms(10));
            assert!(rest.windows[&ws(2)].parking.parked, "placed first: {placed}");
            assert_eq!(rest.due(), None, "placed first: {placed}");
        }
    }

    fn reorder(id: u32) -> ServerOrder { ServerOrder { window: ws(id), change: OrderChange::Reordered, level: Some(LEVEL) } }

    /// The frames rini's writes make, the 808s of a raise, the key window's turn and a new window's
    /// introduction come under the overlay: no turn draws, and the lift draws what came, once.
    #[test]
    fn a_flight_under_the_overlay_draws_nothing_until_its_lift_draws_it_once() {
        let now = Instant::now();
        let mut rest = rest_with(vec![at(1, true, false), at(2, false, false)], SLOT, now);
        rest.cover(Some(display()[0].bounds));
        let elsewhere = rect(868.0, 35.0, 859.0, 1079.0);
        let opened = model(vec![at(1, false, false), at(2, true, false), at(3, false, false)]);
        assert_eq!(rest.ask(&opened), vec![ws(3)], "asked as its model comes, covered");
        let turns = [
            rest.turn(None, &[], &[(ws(1), elsewhere)], Vec::new(), now),
            rest.turn(None, &[], &[(ws(2), SLOT), (ws(1), elsewhere)], vec![reorder(1), reorder(2)], now),
            rest.turn(Some(opened), &[], &[], vec![reorder(2)], now),
            rest.turn(None, &[(ws(3), intro(SLOT))], &[], Vec::new(), now),
        ];
        assert_eq!(turns, [None, None, None, None], "no turn under the overlay draws");
        assert_eq!(rest.settle(now + PARK_PATIENCE), None, "nor does a park's patience");
        assert_eq!(rest.windows[&ws(1)].frame, Some(elsewhere), "the frames are taken as they come");
        assert!(rest.model.windows[0].focused && !rest.windows.contains_key(&ws(3)), "the word waits for the lift");
        let lifted = rest.lift(Some(display()[0].bounds), Some(now), now);
        assert_eq!(lifted, Draw { orders: vec![reorder(1), reorder(2), reorder(2)], focus: Some(ws(2)) });
        assert!(rest.model.windows[1].focused && rest.windows.contains_key(&ws(3)), "taken at the lift, the new window known");
        assert!(!rest.covered());
        let after = rest.turn(None, &[], &[(ws(1), SLOT)], vec![reorder(1)], now);
        assert_eq!(after, Some(Draw { orders: vec![reorder(1)], focus: None }), "after the lift a turn draws again");
    }

    /// A window first named by a flight's model is introduced before the flight writes its frame:
    /// the write's move and a hide that come under the cover are kept with its introduction, and the
    /// place draws it where the window server has it last.
    #[test]
    fn a_window_introduced_under_the_overlay_takes_the_frames_and_orders_that_came_after() {
        let now = Instant::now();
        let written = rect(868.0, 35.0, 859.0, 1079.0);
        for (orders, out) in [(Vec::new(), false), (vec![ServerOrder { window: ws(3), change: OrderChange::Hidden, level: None }], true)] {
            let mut rest = rest_with(vec![at(1, true, false), at(2, false, false)], SLOT, now);
            rest.cover(Some(display()[0].bounds));
            let opened = model(vec![at(1, false, false), at(2, false, false), at(3, true, false)]);
            assert_eq!(rest.ask(&opened), vec![ws(3)]);
            assert_eq!(rest.turn(Some(opened), &[(ws(3), intro(SLOT))], &[], Vec::new(), now), None);
            let raised = StackLevel { level: Some(3), sub_level: Some(1) };
            let reordered = ServerOrder { window: ws(3), change: OrderChange::Reordered, level: Some(raised) };
            let came = [vec![reordered], orders].concat();
            assert_eq!(rest.turn(None, &[], &[(ws(3), written)], came, now + ms(40)), None);
            rest.place(Some(display()[0].bounds), Some(now + ms(300)), now + ms(300), |_| false);
            let Tracked { frame, ordered_out, level, .. } = rest.windows[&ws(3)];
            assert_eq!((frame, ordered_out, level), (Some(written), out, raised), "hidden: {out}");
            assert_eq!(rest.wanted().contains(&ws(3)), !out);
        }
    }

    fn ms(ms: u64) -> Duration { Duration::from_millis(ms) }

    /// The press's own model and the key window's report come before its overlay is up: from the
    /// flight's begin they wait, as frame zero shows the windows before the press, and its lift draws
    /// them.
    #[test]
    fn a_flight_handed_over_holds_the_press_model_until_its_lift() {
        let now = Instant::now();
        let mut rest = rest_with(vec![at(1, true, false), at(2, false, false)], SLOT, now);
        rest.fly(display()[0].bounds, now, now);
        assert!(rest.covered());
        let pressed = model(vec![at(1, false, false), at(2, true, false)]);
        assert_eq!(rest.turn(Some(pressed), &[], &[], vec![reorder(2)], now + ms(1)), None, "the press's model");
        assert_eq!(rest.turn(None, &[], &[(ws(1), SLOT)], Vec::new(), now + ms(20)), None, "a frame there");
        rest.cover(Some(display()[0].bounds));
        assert!(rest.model.windows[0].focused, "the band shows the focus before the press");
        let lifted = rest.lift(Some(display()[0].bounds), Some(now + ms(400)), now + ms(400));
        assert_eq!(lifted, Draw { orders: vec![reorder(2)], focus: Some(ws(2)) });
        assert!(!rest.covered() && rest.model.windows[1].focused);
    }

    /// A flight the engine declines or merges elsewhere shows no overlay and sends no lift: what it
    /// held is drawn once its patience is spent.
    #[test]
    fn a_flight_that_never_shows_lets_go_after_its_patience() {
        let now = Instant::now();
        let mut rest = rest_with(vec![at(1, true, false), at(2, false, false)], SLOT, now);
        rest.fly(display()[0].bounds, now, now);
        let pressed = model(vec![at(1, false, false), at(2, true, false), at(3, false, false)]);
        rest.ask(&pressed);
        assert_eq!(rest.turn(Some(pressed), &[], &[], vec![reorder(2)], now + ms(1)), None);
        assert_eq!(rest.turn(None, &[(ws(3), intro(SLOT))], &[], Vec::new(), now + ms(2)), None);
        assert_eq!(rest.due(), Some(now + FLIGHT_PATIENCE));
        assert_eq!(rest.settle(now + FLIGHT_PATIENCE - ms(1)), None, "patient yet");
        let drawn = rest.settle(now + FLIGHT_PATIENCE);
        assert_eq!(drawn, Some(Draw { orders: vec![reorder(2)], focus: Some(ws(2)) }), "drawn with what waited");
        assert!(!rest.covered() && rest.windows.contains_key(&ws(3)) && rest.due().is_none());

        rest.fly(display()[0].bounds, now, now);
        rest.off();
        assert_eq!(rest.turn(Some(model(vec![at(1, true, false)])), &[], &[], Vec::new(), now), None, "turned on under the flight");
        let drawn = rest.settle(now + FLIGHT_PATIENCE);
        assert_eq!(drawn, Some(Draw { orders: Vec::new(), focus: Some(ws(1)) }), "its patience kept through off");
        let mut off = Rest::default();
        off.fly(display()[0].bounds, now, now);
        assert!(!off.covered() && off.due().is_none(), "off, a flight takes nothing");
    }

    /// A show later than the patience is rare; one that comes in time holds the rest windows to its
    /// lift, as does a flight merged into it.
    #[test]
    fn a_show_after_the_flight_began_keeps_it_covered_past_the_patience() {
        let now = Instant::now();
        let mut rest = rest_with(vec![at(1, true, false), at(2, false, false)], SLOT, now);
        rest.fly(display()[0].bounds, now, now);
        rest.cover(Some(display()[0].bounds));
        let pressed = model(vec![at(1, false, false), at(2, true, false)]);
        assert_eq!(rest.turn(Some(pressed), &[], &[], Vec::new(), now + ms(100)), None);
        rest.fly(display()[0].bounds, now + ms(200), now + ms(200));
        assert_eq!(rest.due(), None, "the show's lift ends it");
        assert_eq!(rest.settle(now + FLIGHT_PATIENCE * 2), None);
        assert!(rest.covered() && rest.model.windows[0].focused);
        let lifted = rest.lift(Some(display()[0].bounds), Some(now + ms(600)), now + ms(600));
        assert_eq!(lifted.focus, Some(ws(2)));
    }

    /// Once the flight has landed, a tick before its lift, the place draws what waited, reading
    /// again what it draws there, and the rest windows stay covered until the lift.
    #[test]
    fn a_place_ahead_of_the_lift_draws_what_waited_and_stays_covered() {
        let now = Instant::now();
        let mut rest = rest_with(vec![at(1, true, false), at(2, false, false)], SLOT, now);
        rest.fly(display()[0].bounds, now, now);
        rest.cover(Some(display()[0].bounds));
        let pressed = model(vec![at(1, false, false), at(2, true, false)]);
        assert_eq!(rest.turn(Some(pressed), &[], &[], vec![reorder(2)], now + ms(1)), None);
        let mut placed = rest.place(Some(display()[0].bounds), Some(now + ms(380)), now + ms(380), |window| window == ws(2));
        placed.reread.sort_by_key(|window| window.as_u32());
        assert_eq!(placed, Placed { draw: Draw { orders: vec![reorder(2)], focus: Some(ws(2)) }, reread: vec![ws(2)] });
        assert!(rest.covered() && rest.model.windows[1].focused, "placed, still under the overlay");
        let moved = rect(868.0, 35.0, 859.0, 1079.0);
        assert_eq!(rest.turn(None, &[], &[(ws(2), moved)], vec![reorder(1)], now + ms(390)), None, "a turn after it waits for the lift");
        assert_eq!(rest.settle(now + ms(390) + PARK_PATIENCE), None);
    }

    /// A press as a flight lifts can begin a flight of its own after the engine gave that lift, and
    /// be heard first: the lift, and a place in its drain, leave that press's model waiting and hand
    /// the cover to its flight, which its own show and lift, or its patience from its begin, end.
    #[test]
    fn a_lift_given_before_a_flight_began_hands_the_cover_to_that_flight() {
        let now = Instant::now();
        let bounds = display()[0].bounds;
        let pressed = model(vec![at(1, false, false), at(2, true, false)]);
        for placed_ahead in [true, false] {
            let mut rest = rest_with(vec![at(1, true, false), at(2, false, false)], SLOT, now);
            rest.fly(bounds, now, now);
            rest.cover(Some(bounds));
            if placed_ahead {
                rest.place(Some(bounds), Some(now), now + ms(380), |_| true);
            }
            let next = now + ms(395);
            rest.fly(bounds, next, next + ms(1));
            assert_eq!(rest.turn(Some(pressed.clone()), &[], &[], vec![reorder(2)], next + ms(1)), None);
            if !placed_ahead {
                let placed = rest.place(Some(bounds), Some(now), next + ms(5), |_| true);
                assert_eq!(placed.draw, Draw::default(), "placed in the lift's drain, the next press waits");
            }
            assert_eq!(rest.lift(Some(bounds), Some(now), next + ms(5)), Draw::default(), "{placed_ahead}");
            assert!(rest.covered() && rest.model.windows[0].focused, "the band shows the focus before the next press");
            assert_eq!(rest.due(), Some(next + ms(1) + FLIGHT_PATIENCE), "the next flight's patience");
            rest.cover(Some(bounds));
            assert_eq!(rest.due(), None, "its show");
            let lifted = rest.lift(Some(bounds), Some(next), next + ms(400));
            assert_eq!(lifted, Draw { orders: vec![reorder(2)], focus: Some(ws(2)) }, "its lift");
            assert!(!rest.covered());
        }
    }

    /// The lift after a place reads nothing: the place read what it draws, and each frame since came
    /// to a covered turn. It draws only what came since the place.
    #[test]
    fn a_lift_after_a_place_reads_nothing_again() {
        let now = Instant::now();
        let mut rest = rest_with(vec![at(1, true, false), at(2, false, false)], SLOT, now);
        rest.cover(Some(display()[0].bounds));
        rest.turn(Some(model(vec![at(1, false, false), at(2, true, false)])), &[], &[], vec![reorder(2)], now);
        rest.place(Some(display()[0].bounds), Some(now), now, |_| true);
        let moved = rect(868.0, 35.0, 859.0, 1079.0);
        let back = model(vec![at(1, true, false), at(2, false, false)]);
        assert_eq!(rest.turn(Some(back), &[], &[(ws(2), moved)], vec![reorder(1)], now + ms(16)), None);
        let lifted: Draw = rest.lift(Some(display()[0].bounds), Some(now + ms(17)), now + ms(17));
        assert_eq!(lifted, Draw { orders: vec![reorder(1)], focus: Some(ws(1)) }, "what came since the place");
        assert_eq!(rest.windows[&ws(2)].frame, Some(moved), "the frame a covered turn took");
        assert!(!rest.covered());
    }

    /// A park is taken with its window's move under the overlay too, and a hide from its order, so
    /// the lift draws neither band.
    #[test]
    fn under_the_overlay_a_park_is_taken_with_its_move_and_a_hide_with_its_order() {
        let now = Instant::now();
        let mut rest = rest_with(vec![at(1, true, false), at(2, false, false)], SLOT, now);
        rest.take_model(model(vec![at(1, true, true), at(2, false, false)]), now);
        rest.cover(Some(display()[0].bounds));
        let hidden = ServerOrder { window: ws(2), change: OrderChange::Hidden, level: None };
        rest.turn(None, &[], &[(ws(1), rect(1676.0, 35.0, 1148.0, 1079.0))], vec![hidden], now);
        assert!(rest.wanted().is_empty());
        let other = rect(1728.0, 0.0, 1920.0, 1080.0);
        assert_eq!(rest.lift(Some(other), Some(now), now).orders, vec![hidden]);
    }

    const OTHER: ScreenRect = ScreenRect::new(Point::new(1728.0, 0.0), Size::new(1920.0, 1080.0));

    /// Two displays side by side, the first's window 1 and the other's window 2, with what `held`
    /// holds each; the overlay is over the first.
    fn two_displays(now: Instant, held: [bool; 2]) -> Rest {
        let displays = vec![display()[0], RestDisplay { bounds: OTHER, scale: 1.0 }];
        let there = rect(1800.0, 35.0, 800.0, 1000.0);
        let mut rest = Rest::default();
        let frames = |window: WindowServerId| if window == ws(2) { there } else { SLOT };
        take_known(&mut rest, RestModel { displays: displays.clone(), windows: vec![at(1, true, false), at(2, false, false)] }, now, frames);
        rest.take_model(RestModel { displays, windows: vec![at(1, true, held[0]), at(2, false, held[1])] }, now);
        rest.cover(Some(display()[0].bounds));
        rest
    }

    /// The overlay covers its own display only: a window on another one moving or closing under it
    /// is drawn at once, with what waited, and a park there settling too.
    #[test]
    fn a_window_on_another_display_is_drawn_while_the_overlay_covers_the_first() {
        let now = Instant::now();
        let mut rest = two_displays(now, [false, false]);
        assert_eq!(rest.turn(None, &[], &[(ws(1), rect(868.0, 35.0, 859.0, 1079.0))], vec![reorder(1)], now), None);
        let refocused = RestModel { windows: vec![at(1, false, false), at(2, true, false)], ..rest.model.clone() };
        assert_eq!(rest.turn(Some(refocused), &[], &[], Vec::new(), now), None, "the word waits");
        let drawn = rest.turn(None, &[], &[(ws(2), rect(1900.0, 35.0, 800.0, 1000.0))], vec![reorder(2)], now);
        assert_eq!(drawn, Some(Draw { orders: vec![reorder(1), reorder(2)], focus: Some(ws(2)) }), "drawn with what waited");
        assert!(rest.covered(), "still under the overlay on its own display");
        let closed = ServerOrder { window: ws(2), change: OrderChange::Closed, level: None };
        assert!(rest.turn(None, &[], &[], vec![closed], now).is_some(), "a close there");

        assert_eq!(two_displays(now, [true, false]).settle(now + PARK_PATIENCE), None, "a park on the overlay's display settles unseen");
        assert!(two_displays(now, [false, true]).settle(now + PARK_PATIENCE).is_some(), "one on the other display is drawn");
    }

    /// A park that came under the overlay for a window off its display waits for its patience past
    /// the lift, so the lift asks for the turn that sets its timer.
    #[test]
    fn a_park_the_lift_takes_no_move_for_wants_a_turn_for_its_patience() {
        let now = Instant::now();
        let mut rest = two_displays(now, [false, false]);
        let parks = RestModel { windows: vec![at(1, true, false), at(2, false, true)], ..rest.model.clone() };
        assert_eq!(rest.turn(Some(parks), &[], &[], Vec::new(), now), None);
        let pool = rest.pool_size();
        rest.lift(Some(display()[0].bounds), Some(now), now);
        assert!(rest.windows[&ws(2)].parking.due().is_some());
        assert!(rest.wants_turn(pool), "a full pool, and a park to time");
        rest.settle(now + PARK_PATIENCE);
        assert!(!rest.wants_turn(pool));
        assert!(rest.wants_turn(pool - 1), "a pool short");
    }

    #[test]
    fn a_turn_draws_at_once_while_nothing_covers_it() {
        let now = Instant::now();
        let mut rest = Rest::default();
        let drawn = rest.turn(Some(model(vec![at(1, true, false)])), &[], &[], vec![reorder(1)], now);
        assert_eq!(drawn, Some(Draw { orders: vec![reorder(1)], focus: Some(ws(1)) }));
        assert_eq!(rest.settle(now), Some(Draw::default()));
        assert_eq!(rest.lift(None, Some(now), now), Draw::default(), "laid out on no display, a lift takes no park");
    }

    /// With the ring and the border off nothing is decorated: the rest windows want no window of
    /// their own, and take no frame, order, place, lift or park until a model turns them on again.
    #[test]
    fn off_it_wants_no_rest_window_and_takes_nothing_until_a_model_comes() {
        let now = Instant::now();
        let mut rest = Rest::default();
        assert!(!rest.on() && rest.pool_size() == 0, "off as it starts");
        assert_eq!(rest.turn(None, &[], &[(ws(1), SLOT)], vec![reorder(1)], now), None);
        let mut rest = rest_with(vec![at(1, true, false), at(2, false, true)], SLOT, now);
        assert!(rest.on() && rest.pool_size() == 2);
        rest.off();
        assert_eq!((rest.on(), rest.pool_size(), rest.windows.len()), (false, 0, 0), "every rest window let go");
        assert_eq!(rest.model, RestModel::default());
        assert_eq!(rest.turn(None, &[], &[(ws(1), SLOT)], vec![reorder(1)], now), None, "a frame and a reorder");
        assert!(rest.windows.is_empty(), "the frame not taken");
        assert!(rest.settle(now + PARK_PATIENCE).is_none() && rest.due().is_none());
        assert_eq!(rest.lift(Some(display()[0].bounds), Some(now), now), Draw::default());
        assert_eq!(rest.place(Some(display()[0].bounds), Some(now), now, |_| true), Placed::default());
        assert!(!rest.wants_turn(0), "a lift asks no turn");
        let drawn = rest.turn(Some(model(vec![at(1, true, false)])), &[], &[], Vec::new(), now);
        assert_eq!(drawn, Some(Draw { orders: Vec::new(), focus: Some(ws(1)) }), "a model turns it on again");
        assert_eq!(rest.pool_size(), 2, "its band and the spare");
    }

    /// Turned on by a config reload during a flight, the rest windows wait for the lift to draw.
    #[test]
    fn a_model_turning_it_on_under_the_overlay_waits_for_the_lift() {
        let now = Instant::now();
        let mut rest = rest_with(vec![at(1, true, false)], SLOT, now);
        rest.cover(Some(display()[0].bounds));
        rest.turn(None, &[], &[], vec![reorder(1)], now);
        rest.off();
        assert!(rest.covered(), "the overlay is still up");
        assert_eq!(rest.turn(Some(model(vec![at(1, true, false)])), &[], &[], Vec::new(), now), None);
        assert!(!rest.on() && rest.pool_size() == 0, "nothing made under the overlay");
        let lifted = rest.lift(Some(display()[0].bounds), Some(now), now);
        assert_eq!(lifted, Draw { orders: Vec::new(), focus: Some(ws(1)) }, "the reorder from before it went off dropped");
        assert!(rest.on() && rest.pool_size() == 2);
    }

    #[test]
    fn a_focus_change_retargets_a_rest_window_from_the_pool() {
        let pool = [Some(ws(1)), Some(ws(2))];
        let focus_moved = assign(&pool, &pool, &[ws(3), ws(2)]);
        assert_eq!(focus_moved, Assignment { retargeted: vec![(0, ws(3))], released: vec![] });
    }

    /// With the ring alone a workspace shows one band, so its rest window and the spare let a
    /// workspace switch's leaving and arriving windows, shown together for a moment, both have one.
    #[test]
    fn a_workspace_switch_finds_a_rest_window_for_every_band_in_the_pool() {
        let now = Instant::now();
        let mut rest = rest_with(vec![at(1, true, false), at(2, false, true)], SLOT, now);
        assert_eq!(rest.pool_size(), 2);
        rest.take_model(model(vec![at(1, false, true), at(2, true, false)]), now);
        rest.take_frames(&[(ws(2), SLOT)], now);
        assert_eq!(rest.wanted(), vec![ws(1), ws(2)], "the arriving window's return before the leaving one's park");
        let pool = vec![None; rest.pool_size()];
        assert_eq!(assign(&pool, &pool, &rest.wanted()).retargeted.len(), 2);
        let held = Held { mission_control: true, ..Held::default() };
        rest.take_model(model(vec![RestWindow { held, ..at(1, true, false) }]), now);
        assert_eq!(rest.pool_size(), 1, "the spare alone while nothing could show");
    }

    /// The pool is the bands the fullest workspace on each display shows, held by no more than a
    /// park, and one spare per display: a display shows one workspace at a time.
    #[test]
    fn the_pool_holds_the_most_bands_one_workspace_shows_and_a_spare_per_display() {
        let now = Instant::now();
        let ring_alone: Vec<RestWindow> = (1..=6).map(|id| at(id, id == 1, id != 1)).collect();
        assert_eq!(rest_with(ring_alone.clone(), SLOT, now).pool_size(), 2, "six workspaces, a ring on each");

        let bordered = |id: u32, workspace: u64| RestWindow {
            decoration: Decoration { border: Some(BORDER), ..Decoration::default() },
            ring: Ring::None,
            group: group(workspace),
            ..at(id, false, workspace != 1)
        };
        let fullscreen = RestWindow { held: Held { fullscreen: true, ..Held::default() }, ..bordered(7, 2) };
        let borders: Vec<RestWindow> =
            (1..=4).map(|id| bordered(id, 1)).chain([bordered(5, 2), bordered(6, 2), fullscreen]).collect();
        assert_eq!(rest_with(borders, SLOT, now).pool_size(), 5, "borders on four windows and on two");

        let mut rest = Rest::default();
        let elsewhere = |id: u32, workspace: u64| RestWindow { group: Group { space: SpaceId::new(2), workspace }, ..at(id, false, workspace != 7) };
        let windows = ring_alone.into_iter().chain([elsewhere(7, 7), elsewhere(8, 8)]).collect();
        let displays = vec![display()[0], RestDisplay { bounds: OTHER, scale: 1.0 }];
        take_known(&mut rest, RestModel { displays, windows }, now, |_| SLOT);
        assert_eq!(rest.pool_size(), 4, "two displays, the ring alone");
    }

    #[test]
    fn a_rest_window_is_retargeted_from_the_pool_while_one_is_left() {
        let pool = [None, Some(ws(9))];
        let assignment = assign(&pool, &pool, &[ws(1), ws(2), ws(3)]);
        assert_eq!(assignment.retargeted, vec![(0, ws(1)), (1, ws(2))]);
        assert!(assignment.released.is_empty());
    }

    #[test]
    fn a_window_that_shows_nothing_returns_its_panel_at_alpha_zero() {
        let pool = [Some(ws(1)), Some(ws(2)), None];
        assert_eq!(assign(&pool, &pool, &[ws(2)]), Assignment { retargeted: vec![], released: vec![0] });
    }

    /// Focus going back and forth between two windows: the ring's rest window on the one it leaves
    /// is kept for it, and the spare that last drew the one it comes to is given back to it, so
    /// neither is laid nor moved again.
    #[test]
    fn a_free_rest_window_goes_back_to_the_window_it_last_drew() {
        let pool = [Some(ws(1)), None];
        let drew = [Some(ws(1)), Some(ws(2))];
        assert_eq!(assign(&pool, &drew, &[ws(2)]), Assignment { retargeted: vec![(1, ws(2))], released: vec![0] });
        let pool = [None, Some(ws(2))];
        assert_eq!(assign(&pool, &drew, &[ws(1)]), Assignment { retargeted: vec![(0, ws(1))], released: vec![1] });
    }

    /// A window no free rest window drew takes one that drew nothing, then one free before the turn,
    /// and the one its band left this turn last: that one keeps the band focus is likeliest to come
    /// back to.
    #[test]
    fn a_window_no_rest_window_drew_takes_the_one_free_longest() {
        let pool = [Some(ws(1)), None, None];
        assert_eq!(assign(&pool, &[Some(ws(1)), Some(ws(3)), None], &[ws(4)]).retargeted, vec![(2, ws(4))], "one that drew nothing");
        assert_eq!(assign(&pool, &[Some(ws(1)), Some(ws(3)), Some(ws(5))], &[ws(4)]).retargeted, vec![(1, ws(4))], "one free before the turn");
        assert_eq!(assign(&[Some(ws(1))], &[Some(ws(1))], &[ws(4)]), Assignment { retargeted: vec![(0, ws(4))], released: vec![] }, "the one freed now, when it is all there is");
    }

    /// A short pool gives its rest windows to the first in `wanted`, whatever each last drew.
    #[test]
    fn a_short_pool_gives_what_it_has_in_wanted_order_before_what_each_drew() {
        let pool = [None];
        assert_eq!(assign(&pool, &[Some(ws(2))], &[ws(1), ws(2)]).retargeted, vec![(0, ws(1))]);
        let pool = [None, None];
        assert_eq!(assign(&pool, &[Some(ws(2)), Some(ws(1))], &[ws(1), ws(2), ws(3)]).retargeted, vec![(1, ws(1)), (0, ws(2))]);
    }

    /// The ring moving with focus retargets the one rest window shown: it shows the window focus came
    /// to afresh, as does a rest window shown again, and one that keeps its window moves with it.
    #[test]
    fn a_rest_window_given_a_window_or_shown_again_shows_it_afresh() {
        let pool = [(Some(ws(3)), true), (Some(ws(2)), false), (Some(ws(1)), true), (Some(ws(4)), false), (None, true)];
        assert_eq!(afresh(&pool, &[0], &[ws(1), ws(2), ws(3)]), vec![0, 1], "retargeted, and shown again");
        assert_eq!(afresh(&pool, &[3], &[ws(1)]), Vec::<usize>::new(), "given a window that shows nothing");
    }

    const CLEAR: ScreenRect = ScreenRect::new(Point::new(100.0, 100.0), Size::new(800.0, 600.0));

    fn look_at(window: &RestWindow, frame: ScreenRect) -> Look {
        Look::of(window, frame, Some(16.0), display()[0]).expect("on the display")
    }

    /// A turn whose window neither moved nor changed what it wears, a reorder's or a focus report's
    /// for another window, draws nothing: each change is main-thread and window-server time.
    #[test]
    fn an_unchanged_window_changes_nothing() {
        let look = look_at(&ringed_window(), CLEAR);
        assert_eq!(delta(Some(look), look_at(&ringed_window(), CLEAR)), Delta::default());
        assert_eq!(delta(Some(look.hidden()), look.hidden()), Delta::default(), "hidden, and hidden still");
        assert_eq!(Look::of(&ringed_window(), rect(4000.0, 100.0, 800.0, 600.0), None, display()[0]), None, "off its display");
    }

    #[test]
    fn a_moved_window_changes_only_its_frame() {
        let look = look_at(&ringed_window(), CLEAR);
        let moved = look_at(&ringed_window(), rect(400.0, 300.0, 800.0, 600.0));
        assert_eq!(delta(Some(look), moved), Delta { frame: true, ..Delta::default() });
        let cut = look_at(&ringed_window(), rect(2.0, 100.0, 800.0, 600.0));
        assert_eq!(
            delta(Some(look), cut),
            Delta { layers: true, frame: true, alpha: false },
            "at the display's edge the band is cut, and the window sits elsewhere in its rest window"
        );
    }

    #[test]
    fn a_resize_a_paint_or_a_scale_lays_its_layers_again() {
        let window = ringed_window();
        let look = look_at(&window, CLEAR);
        let resized = look_at(&window, rect(100.0, 100.0, 900.0, 600.0));
        assert_eq!(delta(Some(look), resized), Delta { layers: true, frame: true, alpha: false }, "resized");
        let layers = Delta { layers: true, ..Delta::default() };
        let inactive = RestWindow { focused: false, ring: Ring::Inactive, ..window };
        assert_eq!(delta(Some(look), look_at(&inactive, CLEAR)), layers, "focus left it");
        let bordered = RestWindow { decoration: Decoration { border: Some(BORDER), ..window.decoration }, ..window };
        assert_eq!(delta(Some(look), look_at(&bordered, CLEAR)), Delta { layers: true, frame: true, alpha: false }, "a border on");
        let rounded = Look::of(&window, CLEAR, Some(10.0), display()[0]).expect("on the display");
        assert_eq!(delta(Some(look), rounded), layers, "another corner");
        let single = RestDisplay { scale: 1.0, ..display()[0] };
        let scaled = Look::of(&window, CLEAR, Some(16.0), single).expect("on the display");
        assert_eq!(delta(Some(look), scaled), layers, "another scale");
    }

    /// A rest window keeps its layers and its frame at alpha 0: shown again, it takes its alpha, and
    /// lays and moves only for what changed while it was hidden.
    #[test]
    fn a_window_shown_again_lays_only_what_changed_while_it_was_hidden() {
        let window = ringed_window();
        let look = look_at(&window, CLEAR);
        let alpha = Delta { alpha: true, ..Delta::default() };
        assert_eq!(delta(Some(look), look.hidden()), alpha, "hidden");
        assert_eq!(delta(Some(look.hidden()), look), alpha, "shown again as it was");
        let elsewhere = look_at(&RestWindow { ring: Ring::Inactive, ..window }, rect(400.0, 100.0, 900.0, 600.0));
        let all = Delta { layers: true, frame: true, alpha: true };
        assert_eq!(delta(Some(look.hidden()), elsewhere), all, "resized, moved and repainted meanwhile");
        assert_eq!(delta(None, look), all, "a rest window just made");
    }

    #[test]
    fn a_reorder_and_a_focus_change_reorder_their_panel_once_per_turn() {
        let targets = [Some(ws(1)), Some(ws(2)), None];
        let order = |id, change| ServerOrder { window: ws(id), change, level: None };
        let orders = [
            order(1, OrderChange::Reordered),
            order(1, OrderChange::Reordered),
            order(2, OrderChange::Hidden),
            order(7, OrderChange::Reordered),
        ];
        assert_eq!(to_reorder(&orders, &targets, &[0]), vec![0], "once, focus and 808s together");
        assert_eq!(to_reorder(&[order(2, OrderChange::Unhidden)], &targets, &[]), vec![1], "ordered back in");
    }

    #[test]
    fn a_window_is_drawn_on_the_display_holding_most_of_it() {
        let two = [display()[0], RestDisplay { bounds: rect(1728.0, 0.0, 1920.0, 1080.0), scale: 1.0 }];
        assert_eq!(display_of(rect(1500.0, 35.0, 859.0, 1079.0), &two), Some(two[1]));
        assert_eq!(display_of(rect(1000.0, 35.0, 859.0, 1079.0), &two), Some(two[0]));
        assert_eq!(display_of(rect(4000.0, 35.0, 859.0, 1079.0), &two), None);
    }
}
