//! A decoration at rest, drawn by a window of rini's own directly above the window it decorates:
//! which windows show a band and when, what each turn of the rest windows makes of the reactor's
//! word and the window server's, which of rini's rest windows draws each, and which are ordered above
//! their windows again. See "At rest" in `src/decorations/docs/README.md`.

use std::time::{Duration, Instant};

use rini_core::ids::WindowServerId;
use rini_geometry::space::ScreenRect;
use rustc_hash::FxHashMap as HashMap;

use crate::decorations::domain::shape;
use crate::decorations::domain::style::{Decoration, Ring, Style};
use crate::displays::event::{OrderChange, ServerOrder};

/// How long a change of `parked` waits for its window to move before it takes effect anyway.
pub const PARK_PATIENCE: Duration = Duration::from_millis(250);

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

/// One window that wears a decoration rini draws at rest, as the reactor sends it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RestWindow {
    pub window: WindowServerId,
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

/// What is known of each decorated window between the reactor's words.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tracked {
    pub frame: Option<ScreenRect>,
    pub parking: Parking,
    pub ordered_out: bool,
}

/// What the rest windows draw from: the reactor's latest model, and what is known of each window it
/// names. Each turn takes what queued: the model, the window server's frames and orders, a lift.
#[derive(Clone, Debug, Default)]
pub struct Rest {
    pub model: RestModel,
    pub windows: HashMap<WindowServerId, Tracked>,
}

impl Rest {
    /// The reactor's word at `now`: a window new to it is read off the window server (`read`, its
    /// frame and whether it is ordered out), each park waits for its window's move, and the window
    /// focus came to is named, when it changed, so its rest window is ordered again.
    pub fn take_model(
        &mut self,
        model: RestModel,
        now: Instant,
        read: impl Fn(WindowServerId) -> (Option<ScreenRect>, bool),
    ) -> Option<WindowServerId> {
        let focus_before = self.focus();
        self.windows.retain(|window, _| model.windows.iter().any(|w| w.window == *window));
        for window in &model.windows {
            let tracked = self.windows.entry(window.window).or_insert_with(|| {
                let (frame, ordered_out) = read(window.window);
                Tracked { frame, parking: Parking::new(window.held.parked), ordered_out }
            });
            tracked.parking.want(window.held.parked, now);
        }
        self.model = model;
        self.focus().filter(|focus| Some(*focus) != focus_before)
    }

    fn focus(&self) -> Option<WindowServerId> {
        self.model.windows.iter().find(|window| window.focused).map(|window| window.window)
    }

    /// Where the window server has windows that moved, each latest last: a waiting park is taken
    /// with its window's move.
    pub fn take_frames(&mut self, frames: &[(WindowServerId, ScreenRect)], now: Instant) {
        for (window, frame) in frames {
            if let Some(tracked) = self.windows.get_mut(window) {
                tracked.frame = Some(*frame);
                tracked.parking.settle(true, false, now);
            }
        }
    }

    pub fn take_orders(&mut self, orders: &[ServerOrder]) {
        for (window, tracked) in self.windows.iter_mut() {
            tracked.ordered_out = ordered_out(tracked.ordered_out, *window, orders);
        }
    }

    /// The overlay lifts off `display` at `now`: every park waiting on a window there is taken. The
    /// windows whose frame is read again from the window server before the bands are drawn: each one
    /// there brought back, and each one there a rest window draws (`drawn`).
    pub fn lift(&mut self, display: ScreenRect, now: Instant, drawn: impl Fn(WindowServerId) -> bool) -> Vec<WindowServerId> {
        let mut reread = Vec::new();
        for (window, tracked) in self.windows.iter_mut() {
            let there = tracked.frame.is_some_and(|frame| frame.intersects(&display));
            let parked = tracked.parking.parked;
            tracked.parking.settle(false, there, now);
            let brought_back = parked && !tracked.parking.parked;
            if brought_back || (there && drawn(*window)) {
                reread.push(*window);
            }
        }
        reread
    }

    /// `window`'s frame as the window server has it now, kept as it was if it has none.
    pub fn reread(&mut self, window: WindowServerId, frame: Option<ScreenRect>) {
        if let Some(tracked) = self.windows.get_mut(&window) {
            tracked.frame = frame.or(tracked.frame);
        }
    }

    /// Parks whose patience ran out at `now`, taken.
    pub fn settle(&mut self, now: Instant) {
        for tracked in self.windows.values_mut() {
            tracked.parking.settle(false, false, now);
        }
    }

    /// When the next waiting park takes effect.
    pub fn due(&self) -> Option<Instant> { self.windows.values().filter_map(|tracked| tracked.parking.due()).min() }

    /// How many rest windows the pool holds for this model: one for every window that wears a band
    /// and could show it, held by no more than a park, so a park or a return through a flight takes
    /// a rest window from the pool and none is made inside a flight; and one per display at least, so
    /// the ring moving with focus makes none.
    pub fn pool_size(&self) -> usize {
        let could_show = self.model.windows.iter().filter(|window| {
            let Held { parked: _, mission_control, fullscreen } = window.held;
            window.wears_a_band() && !(mission_control || fullscreen)
        });
        could_show.count().max(self.model.displays.len())
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

/// Which rest window draws each window whose band shows.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Assignment {
    /// Rest windows given a window, by their place in the pool: each is ordered above it.
    pub retargeted: Vec<(usize, WindowServerId)>,
    /// Rest windows whose window shows nothing now: drawn at alpha 0, kept for the next.
    pub released: Vec<usize>,
}

/// One rest window for each of `wanted`, from `pool`, each window's position the window it draws if
/// any: a rest window already on a wanted window keeps it, and the others are given one while the
/// pool has one left (`Rest::pool_size`).
pub fn assign(pool: &[Option<WindowServerId>], wanted: &[WindowServerId]) -> Assignment {
    let kept = |target: &Option<WindowServerId>| target.is_some_and(|target| wanted.contains(&target));
    let mut free = pool.iter().enumerate().filter(|(_, target)| !kept(target)).map(|(at, _)| at);
    let mut assignment = Assignment::default();
    for window in wanted.iter().filter(|window| !pool.contains(&Some(**window))) {
        let Some(at) = free.next() else { break };
        assignment.retargeted.push((at, *window));
    }
    assignment.released = free.filter(|at| pool[*at].is_some()).collect();
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

    fn ringed(held: Held) -> RestWindow {
        RestWindow {
            window: ws(56),
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
        let order = |change| ServerOrder { window: ws(56), change };
        let other = ServerOrder { window: ws(57), change: OrderChange::Unhidden };
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

    fn at(id: u32, focused: bool, parked: bool) -> RestWindow {
        RestWindow { window: ws(id), focused, held: Held { parked, ..Held::default() }, ..ringed_window() }
    }

    fn rest_with(windows: Vec<RestWindow>, frame: ScreenRect, now: Instant) -> Rest {
        let mut rest = Rest::default();
        rest.take_model(model(windows), now, |_| (Some(frame), false));
        rest
    }

    #[test]
    fn the_window_focus_comes_to_is_named_and_a_model_with_focus_where_it_was_names_none() {
        let now = Instant::now();
        let mut rest = Rest::default();
        assert_eq!(rest.take_model(model(vec![at(1, true, false), at(2, false, false)]), now, |_| (Some(SLOT), false)), Some(ws(1)));
        assert_eq!(rest.take_model(model(vec![at(1, true, false), at(2, false, true)]), now, |_| (None, false)), None, "focus unchanged");
        assert_eq!(rest.take_model(model(vec![at(1, false, false), at(2, true, false)]), now, |_| (None, false)), Some(ws(2)));
        assert_eq!(rest.take_model(model(vec![at(1, false, false)]), now, |_| (None, false)), None, "no window focused");
        assert_eq!(rest.windows.len(), 1, "a window the reactor no longer names is forgotten");
    }

    #[test]
    fn a_window_new_to_the_rest_windows_is_read_off_the_window_server_once() {
        let now = Instant::now();
        let mut rest = Rest::default();
        rest.take_model(model(vec![at(1, true, false)]), now, |_| (Some(SLOT), true));
        assert_eq!(rest.windows[&ws(1)], Tracked { frame: Some(SLOT), parking: Parking::new(false), ordered_out: true });
        rest.take_model(model(vec![at(1, true, false)]), now, |_| panic!("read again"));
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
        rest.take_model(model(vec![at(1, true, true)]), now, |_| (None, false));
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
        rest.take_orders(&[ServerOrder { window: ws(1), change: OrderChange::Hidden }]);
        assert!(rest.wanted().is_empty());
        rest.take_orders(&[ServerOrder { window: ws(1), change: OrderChange::Unhidden }]);
        assert_eq!(rest.wanted(), vec![ws(1)]);
    }

    /// A window leaves the screen as it closes, before the reactor hears of it: its band goes then.
    #[test]
    fn a_window_the_window_server_closes_shows_no_band_from_its_close() {
        let now = Instant::now();
        let mut rest = rest_with(vec![at(1, true, false)], SLOT, now);
        rest.take_orders(&[ServerOrder { window: ws(1), change: OrderChange::Closed }]);
        assert!(rest.wanted().is_empty());
        let targets = [Some(ws(1))];
        assert_eq!(to_reorder(&[ServerOrder { window: ws(1), change: OrderChange::Closed }], &targets, &[]), Vec::<usize>::new());
    }

    /// A pass that changes two displays parks a window on each; a lift ends the flight on one.
    #[test]
    fn a_lift_takes_the_parks_on_its_display_alone_and_reads_again_what_it_draws_there() {
        let now = Instant::now();
        let other = rect(1728.0, 0.0, 1920.0, 1080.0);
        let mut rest = Rest::default();
        let frames = |window: WindowServerId| match window.as_u32() {
            1 | 3 => (Some(SLOT), false),
            _ => (Some(rect(1731.0, 35.0, 859.0, 1079.0)), false),
        };
        rest.take_model(model(vec![at(1, true, false), at(2, false, false), at(3, false, true)]), now, frames);
        rest.take_model(model(vec![at(1, true, true), at(2, false, true), at(3, false, false)]), now, |_| (None, false));
        let mut reread = rest.lift(display()[0].bounds, now, |window| window == ws(1));
        reread.sort_by_key(|window| window.as_u32());
        assert_eq!(reread, vec![ws(1), ws(3)], "drawn there, and brought back there");
        assert!(rest.windows[&ws(1)].parking.parked && !rest.windows[&ws(3)].parking.parked);
        assert!(!rest.windows[&ws(2)].parking.parked, "a park on another display waits for its own move");
        assert_eq!(rest.lift(other, now, |_| false), Vec::new(), "parked, drawn by none");
        assert!(rest.windows[&ws(2)].parking.parked, "its own display's lift");
        rest.reread(ws(1), None);
        assert_eq!(rest.windows[&ws(1)].frame, Some(SLOT), "no frame read keeps the one known");
    }

    #[test]
    fn a_focus_change_retargets_a_rest_window_from_the_pool() {
        let pool = [Some(ws(1)), Some(ws(2))];
        let focus_moved = assign(&pool, &[ws(3), ws(2)]);
        assert_eq!(focus_moved, Assignment { retargeted: vec![(0, ws(3))], released: vec![] });
    }

    /// The pool holds a rest window for every window that could show its band, so a workspace
    /// switch's leaving and arriving windows, shown together for a moment, both have one.
    #[test]
    fn a_workspace_switch_finds_a_rest_window_for_every_band_in_the_pool() {
        let now = Instant::now();
        let mut rest = rest_with(vec![at(1, true, false), at(2, false, true)], SLOT, now);
        assert_eq!(rest.pool_size(), 2);
        rest.take_model(model(vec![at(1, false, true), at(2, true, false)]), now, |_| (None, false));
        rest.take_frames(&[(ws(2), SLOT)], now);
        assert_eq!(rest.wanted(), vec![ws(1), ws(2)], "the arriving window's return before the leaving one's park");
        let pool = vec![None; rest.pool_size()];
        assert_eq!(assign(&pool, &rest.wanted()).retargeted.len(), 2);
        let held = Held { mission_control: true, ..Held::default() };
        rest.take_model(model(vec![RestWindow { held, ..at(1, true, false) }]), now, |_| (None, false));
        assert_eq!(rest.pool_size(), 1, "one per display while nothing could show");
    }

    #[test]
    fn a_rest_window_is_retargeted_from_the_pool_while_one_is_left() {
        let pool = [None, Some(ws(9))];
        let assignment = assign(&pool, &[ws(1), ws(2), ws(3)]);
        assert_eq!(assignment.retargeted, vec![(0, ws(1)), (1, ws(2))]);
        assert!(assignment.released.is_empty());
    }

    #[test]
    fn a_window_that_shows_nothing_returns_its_panel_at_alpha_zero() {
        let pool = [Some(ws(1)), Some(ws(2)), None];
        assert_eq!(assign(&pool, &[ws(2)]), Assignment { retargeted: vec![], released: vec![0] });
    }

    /// The ring moving with focus retargets the one rest window shown: it shows the window focus came
    /// to afresh, as does a rest window shown again, and one that keeps its window moves with it.
    #[test]
    fn a_rest_window_given_a_window_or_shown_again_shows_it_afresh() {
        let pool = [(Some(ws(3)), true), (Some(ws(2)), false), (Some(ws(1)), true), (Some(ws(4)), false), (None, true)];
        assert_eq!(afresh(&pool, &[0], &[ws(1), ws(2), ws(3)]), vec![0, 1], "retargeted, and shown again");
        assert_eq!(afresh(&pool, &[3], &[ws(1)]), Vec::<usize>::new(), "given a window that shows nothing");
    }

    #[test]
    fn a_reorder_and_a_focus_change_reorder_their_panel_once_per_turn() {
        let targets = [Some(ws(1)), Some(ws(2)), None];
        let order = |id, change| ServerOrder { window: ws(id), change };
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
