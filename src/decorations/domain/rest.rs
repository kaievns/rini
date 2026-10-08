//! A decoration at rest, drawn by a window of rini's own directly above the window it decorates:
//! which windows show a band and when, which of rini's rest windows draws each, and which are
//! ordered above their windows again. See "At rest" in `src/decorations/docs/README.md`.

use std::time::{Duration, Instant};

use rini_core::ids::WindowServerId;
use rini_geometry::space::ScreenRect;

use crate::decorations::domain::shape;
use crate::decorations::domain::style::{Decoration, Style};
use crate::displays::event::{OrderChange, ServerOrder};

/// How long a change of `parked` waits for its window to move before it takes effect anyway.
pub const PARK_PATIENCE: Duration = Duration::from_millis(250);

/// Why a window shows no band wherever it is, as rini's own stores have it.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Held {
    /// rini has it off screen: its workspace is not the one its display shows, or its strip has it
    /// off the viewport. Read from rini's layout, never from the window's frame, of which macOS
    /// leaves up to 52pt on screen.
    pub parked: bool,
    pub mission_control: bool,
    /// In native fullscreen, or entering or leaving it.
    pub fullscreen: bool,
    /// On a Space its display is not showing.
    pub space_hidden: bool,
}

/// One window that wears a decoration rini draws at rest, as the reactor sends it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RestWindow {
    pub window: WindowServerId,
    pub decoration: Decoration,
    /// The managed window macOS has focused.
    pub focused: bool,
    pub ringed: bool,
    pub held: Held,
}

impl RestWindow {
    /// How its bands are painted: those it wears in their paints, the others none.
    pub fn style(&self) -> Style { Style::of(self.focused, self.ringed, &self.decoration) }

    /// Whether it wears a band now: the ring, or a border.
    pub fn wears_a_band(&self) -> bool {
        let style = self.style();
        style.ring.is_some() || style.border.is_some()
    }

    /// How far its bands reach outside it, in points of a display of `scale`.
    pub fn outset(&self, scale: f64) -> f64 { shape::outset(&self.decoration, self.ringed, scale) }
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
    let Held { parked: _, mission_control, fullscreen, space_hidden } = window.held;
    let on_a_display = frame.and_then(|frame| display_of(frame, displays)).is_some();
    window.wears_a_band() && !(parked || mission_control || fullscreen || space_hidden || ordered_out) && on_a_display
}

/// Whether a window is ordered out after `orders`, ordered out before them if `was`: its last hide
/// or unhide among them decides.
pub fn ordered_out(was: bool, window: WindowServerId, orders: &[ServerOrder]) -> bool {
    orders.iter().filter(|order| order.window == window).fold(was, |out, order| match order.change {
        OrderChange::Hidden => true,
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

/// Which rest window draws each window whose band shows.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Assignment {
    /// Rest windows given a window, by their place in the pool: each is ordered above it.
    pub retargeted: Vec<(usize, WindowServerId)>,
    /// Rest windows whose window shows nothing now: drawn at alpha 0, kept for the next.
    pub released: Vec<usize>,
    /// Windows no rest window is left for: one is made for each.
    pub made: Vec<WindowServerId>,
}

/// One rest window for each of `wanted`, from `pool`, each window's position the window it draws if
/// any: a rest window already on a wanted window keeps it, the others are given one, and one is made
/// only when the pool has no rest window left.
pub fn assign(pool: &[Option<WindowServerId>], wanted: &[WindowServerId]) -> Assignment {
    let kept = |target: &Option<WindowServerId>| target.is_some_and(|target| wanted.contains(&target));
    let mut free = pool.iter().enumerate().filter(|(_, target)| !kept(target)).map(|(at, _)| at);
    let mut assignment = Assignment::default();
    for window in wanted.iter().filter(|window| !pool.contains(&Some(**window))) {
        match free.next() {
            Some(at) => assignment.retargeted.push((at, *window)),
            None => assignment.made.push(*window),
        }
    }
    assignment.released = free.filter(|at| pool[*at].is_some()).collect();
    assignment
}

/// How many rest windows to make with a pool of `pool` on `displays` displays: one per display, so
/// the ring moving with focus makes none. Making a window costs about 112ms.
pub fn spare(pool: usize, displays: usize) -> usize { displays.saturating_sub(pool) }

/// The rest windows to order above their windows again this turn, each once: those whose window the
/// window server reordered or ordered back in, and those in `also`, a rest window just given a window
/// or the one whose window focus came to. `targets` is the pool, each one's window if any.
pub fn to_reorder(orders: &[ServerOrder], targets: &[Option<WindowServerId>], also: &[usize]) -> Vec<usize> {
    let moved = orders
        .iter()
        .filter(|order| order.change != OrderChange::Hidden)
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
            ringed: true,
            held,
        }
    }

    const SLOT: ScreenRect = ScreenRect::new(Point::new(3.0, 35.0), Size::new(859.0, 1079.0));

    #[test]
    fn a_ringed_window_on_its_display_shows_its_band() {
        assert!(shown(&ringed(Held::default()), false, false, Some(SLOT), &display()));
        let unringed = RestWindow { ringed: false, ..ringed(Held::default()) };
        assert!(!shown(&unringed, false, false, Some(SLOT), &display()), "it wears nothing");
        assert!(!shown(&ringed(Held::default()), false, false, None, &display()), "no frame yet");
    }

    #[test]
    fn a_bordered_window_shows_its_band_whether_it_wears_the_ring_or_not() {
        let bordered = |ringed| RestWindow {
            decoration: Decoration { border: Some(BORDER), ..ringed_window().decoration },
            ringed,
            focused: false,
            ..ringed_window()
        };
        for ringed in [true, false] {
            assert!(shown(&bordered(ringed), false, false, Some(SLOT), &display()), "ringed {ringed}");
        }
        assert_eq!(bordered(false).style().border, Some(BORDER.inactive));
        assert_eq!((bordered(true).outset(2.0), bordered(false).outset(2.0)), (8.0, 4.0), "the ring outside the border");
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

    #[test]
    fn a_window_on_a_space_its_display_is_not_showing_shows_no_band() {
        let window = ringed(Held { space_hidden: true, ..Held::default() });
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
    fn a_window_followed_through_a_drag_is_shown_throughout() {
        let window = ringed(Held::default());
        for x in [3.0, 120.0, 900.0, 1500.0, -600.0] {
            assert!(shown(&window, false, false, Some(rect(x, 35.0, 859.0, 1079.0)), &display()), "at {x}");
        }
        assert!(!shown(&window, false, false, Some(rect(1800.0, 35.0, 859.0, 1079.0)), &display()), "dragged off every display");
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

    #[test]
    fn the_pool_makes_one_panel_per_display_at_launch_and_none_for_a_focus_change() {
        assert_eq!(spare(0, 2), 2, "one per display at launch");
        assert_eq!(spare(2, 2), 0);
        let pool = [Some(ws(1)), Some(ws(2))];
        let focus_moved = assign(&pool, &[ws(3), ws(2)]);
        assert_eq!(focus_moved, Assignment { retargeted: vec![(0, ws(3))], released: vec![], made: vec![] });
    }

    #[test]
    fn a_panel_is_retargeted_from_the_pool_before_one_is_made() {
        let pool = [None, Some(ws(9))];
        let assignment = assign(&pool, &[ws(1), ws(2), ws(3)]);
        assert_eq!(assignment.retargeted, vec![(0, ws(1)), (1, ws(2))]);
        assert_eq!(assignment.made, vec![ws(3)]);
        assert!(assignment.released.is_empty());
    }

    #[test]
    fn a_window_that_shows_nothing_returns_its_panel_at_alpha_zero() {
        let pool = [Some(ws(1)), Some(ws(2)), None];
        assert_eq!(assign(&pool, &[ws(2)]), Assignment { retargeted: vec![], released: vec![0], made: vec![] });
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
