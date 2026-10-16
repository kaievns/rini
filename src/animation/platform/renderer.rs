//! The renderer: the main thread's actor that owns the tile overlay and plays the render commands the
//! flight engine sends it, a batch an event. The batches queued when it wakes are one drain, played
//! in one `CATransaction`, flushed so its answer means the render server has it; the window's alpha
//! and background, which are not the layers', are set around it. It decides nothing: every motion it
//! plays is an edit of the coordinator's score, begun on the media clock `media_clock` reads. See
//! "One transaction a drain, flushed" in `src/animation/docs/README.md`.

use std::time::Instant;

use objc2::MainThreadMarker;
use objc2_quartz_core::CATransaction;
use rini_core::ids::{WindowId, WindowServerId};
use rini_geometry::space::{OverlayRect, ScreenRect};
use rini_runloop::channel::Receiver;
use tracing::warn;

use crate::animation::domain::motion::score::{Edit, MediaClock, Score};
use crate::animation::event::{Crops, Event, OverlayTurn, Played, RestPlacement};
use crate::animation::platform::overlay::{OverlayTile, TileOverlay};
use crate::decorations::domain::style::Worn;
use crate::decorations::platform::rest::Handover;
use crate::pictures::platform::picture::Picture;

/// Everything the engine asks of the overlay, each one of its calls. Nothing asks for anything back.
pub enum Render {
    /// The display the overlay covers, in Screen space, the band the bar reserves at its top and the
    /// scale.
    Display {
        frame: ScreenRect,
        top_band: f64,
        scale: f64,
    },
    /// The desktop drawn behind the tiles and the bar over them, at `strip`.
    Dress {
        desktop: Option<Picture>,
        bar: Option<Picture>,
        strip: Option<OverlayRect>,
    },
    /// Frame zero, and each pose of it again before the motion, every tile dressed from `tiles`.
    Stage {
        score: Score,
        tiles: Vec<OverlayTile>,
    },
    /// The score's edits: the motion, merges, accents, snaps and banks; a tile first ridden is
    /// dressed from `tiles`.
    Play {
        edits: Vec<Edit>,
        tiles: Vec<OverlayTile>,
    },
    /// A fresh picture onto `window`'s tile, `clipped` to its corner or whole, its movement left
    /// alone.
    Swap {
        window: WindowId,
        picture: Picture,
        clipped: bool,
    },
    /// `window`'s tile drawn focused or not: its hairline and shadow.
    Focus { window: WindowId, focused: bool },
    /// `window`'s tile wearing its ring and border as `worn` has them where the flight now ends.
    Wear { window: WindowId, worn: Worn },
    /// The overlay holed over each window ending at one of these, and over the band of the outset
    /// given that it wears at rest, the holes before replaced.
    Holes(Vec<(OverlayRect, f64)>),
    /// The overlay up.
    Show,
    /// What is drawn at rest on the display put where its windows are, under the overlay, ahead of
    /// the lift, given `through` the latest press the engine had taken.
    PlaceRest { through: Option<Instant> },
    /// The overlay down, its holes filled, and its tiles and the desktop behind them let go, given
    /// `through` the latest press the engine had taken.
    Lift { through: Option<Instant> },
    /// Where each container is presented, logged; sent only while `TRACE_TARGET` is on at trace level.
    Trace,
}

/// The render commands one event of the engine gave, numbered, and when the engine sent them.
pub struct Batch {
    pub seq: u64,
    pub sent: Instant,
    pub commands: Vec<Render>,
}

/// The batches queued, played as one: their commands in order, the highest number, when the oldest
/// was sent, which way it turns the overlay, if it does, whether it places what is drawn at rest,
/// and the press the last place or lift in it was given through.
pub struct Drained {
    pub commands: Vec<Render>,
    pub seq: u64,
    pub sent: Instant,
    pub turn: Option<OverlayTurn>,
    pub rest: RestPlacement,
    pub through: Option<Instant>,
}

/// The batch the renderer woke on and the ones queued behind it, as one drain: whether the overlay
/// is up after it is the last `Show` or `Lift` in it, so a drain that shows and then lifts never
/// shows.
pub fn in_order(woke: Batch, mut queued: Vec<Batch>) -> Drained {
    let seq = queued.iter().fold(woke.seq, |seq, batch| seq.max(batch.seq));
    let sent = queued.iter().fold(woke.sent, |sent, batch| sent.min(batch.sent));
    queued.push(woke);
    queued.sort_by_key(|batch| batch.seq);
    let commands: Vec<Render> = queued.into_iter().flat_map(|batch| batch.commands).collect();
    let turn = commands.iter().rev().find_map(|command| match command {
        Render::Show => Some(OverlayTurn::Shown),
        Render::Lift { .. } => Some(OverlayTurn::Lifted),
        _ => None,
    });
    let rest = if commands.iter().any(|command| matches!(command, Render::PlaceRest { .. })) {
        RestPlacement::Placed
    } else {
        RestPlacement::Unplaced
    };
    let through = commands.iter().rev().find_map(|command| match command {
        Render::PlaceRest { through } | Render::Lift { through } => Some(*through),
        _ => None,
    });
    Drained {
        commands,
        seq,
        sent,
        turn,
        rest,
        through: through.flatten(),
    }
}

/// Whether `command` makes the overlay when there is none: it dresses or poses a flight.
fn needs_overlay(command: &Render) -> bool {
    matches!(command, Render::Dress { .. } | Render::Stage { .. })
}

/// Whether a drain dresses or poses a flight, which draws nothing without the overlay.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OverlayNeed {
    Needed,
    Unneeded,
}

impl OverlayNeed {
    fn of(commands: &[Render]) -> Self {
        if commands.iter().any(needs_overlay) {
            Self::Needed
        } else {
            Self::Unneeded
        }
    }
}

/// The overlay after a drain: none made, made but not yet numbered by the window server, or
/// numbered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OverlayWindow {
    Absent,
    Unnumbered,
    Numbered(WindowServerId),
}

/// What a drain comes to, given the overlay after it: not drawn when it needed the overlay and there
/// is none, else committed, any drain with no overlay included, with the overlay's number once the
/// window server has numbered it and what it `played`.
fn answer(
    seq: u64,
    need: OverlayNeed,
    overlay: OverlayWindow,
    played: Played,
    (sent, began, flushed): (Instant, Instant, Instant),
) -> Event {
    let number = match overlay {
        OverlayWindow::Absent if need == OverlayNeed::Needed => {
            return Event::Unavailable { seq };
        }
        OverlayWindow::Absent | OverlayWindow::Unnumbered => None,
        OverlayWindow::Numbered(id) => Some(id),
    };
    Event::Committed {
        seq,
        overlay: number,
        played,
        sent,
        began,
        flushed,
    }
}

/// Core Animation's media clock against `Instant`, read now: what the engine begins every track on,
/// the tightest of four reads (`MediaClock::tightest`).
pub fn media_clock() -> MediaClock {
    let read = |_| {
        let before = Instant::now();
        let media = objc2_quartz_core::CACurrentMediaTime();
        (before, media, Instant::now())
    };
    MediaClock::tightest((0..4).map(read)).expect("four reads")
}

/// What a drain does around its transaction, in order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Step {
    /// Every window drawn at rest on the display put where its window is, still under the overlay,
    /// so the lift shows each band where the overlay's tile had it.
    PlaceRest,
    /// What is drawn at rest told the overlay lifts: what came since its place is drawn, and from
    /// here it draws as it comes.
    LiftRest,
    Hide,
    /// The commands, in one transaction, flushed.
    Play,
    Background,
    Show,
    /// What is drawn at rest told it is under the overlay, so it draws nothing until it is placed.
    CoverRest,
}

/// A drain's steps: one that places what is drawn at rest puts it where its windows are first; one
/// that lifts the overlay then tells what is drawn at rest and hides it, all before its commands;
/// one that shows it puts it up last and then covers what is drawn at rest.
fn steps(turn: Option<OverlayTurn>, rest: RestPlacement) -> Vec<Step> {
    let place = (rest == RestPlacement::Placed).then_some(Step::PlaceRest);
    let (before, after): (&[Step], &[Step]) = match turn {
        Some(OverlayTurn::Lifted) => (&[Step::LiftRest, Step::Hide], &[]),
        Some(OverlayTurn::Shown) => (&[], &[Step::Show, Step::CoverRest]),
        None => (&[], &[]),
    };
    place
        .into_iter()
        .chain(before.iter().copied())
        .chain([Step::Play, Step::Background])
        .chain(after.iter().copied())
        .collect()
}

pub struct Renderer {
    rx: Receiver<Batch>,
    /// Made by the first `Dress` or `Stage` after a `Display`, and kept: making it costs too much to
    /// pay a flight ("Toggle alpha, do not order the window in and out" in
    /// `src/animation/docs/capture-overlay-research.md`).
    overlay: Option<TileOverlay>,
    /// The display last laid out: the frame, the bar's band and the scale.
    display: Option<(ScreenRect, f64, f64)>,
    /// Tells the engine what each drain came to.
    report: Box<dyn Fn(Event)>,
    rest: Handover,
    mtm: MainThreadMarker,
}

impl Renderer {
    pub fn new(
        rx: Receiver<Batch>,
        report: Box<dyn Fn(Event)>,
        rest: Handover,
        mtm: MainThreadMarker,
    ) -> Self {
        Self {
            rx,
            overlay: None,
            display: None,
            report,
            rest,
            mtm,
        }
    }

    /// Each time a batch comes, it and every batch queued behind it are played as one drain, and
    /// answered once: a busy main thread costs one late commit, never a backlog of them.
    pub async fn run(mut self) {
        while let Some((span, woke)) = self.rx.recv().await {
            let mut queued = Vec::new();
            while let Ok((_, batch)) = self.rx.try_recv() {
                queued.push(batch);
            }
            let _guard = span.enter();
            let answer = self.play(in_order(woke, queued));
            (self.report)(answer);
        }
    }

    /// Plays a drain in one transaction, flushed, in its `steps`: before it, what is drawn at rest on
    /// its display placed when the drain carries `PlaceRest`, and when the drain lifts the overlay,
    /// what is drawn at rest told and the overlay brought down; put up after when the drain shows
    /// it, and what is drawn at rest covered; and the window's background set once the holes' mask
    /// is committed.
    /// Without an overlay every command but `Display` does nothing; a drain that dressed or posed a
    /// flight on none is `Unavailable`. A commit says when the drain began and when it was flushed,
    /// and what its commands spent on crop-drawn tiles.
    fn play(&mut self, drained: Drained) -> Event {
        let began = Instant::now();
        let Drained {
            mut commands,
            seq,
            sent,
            turn,
            rest,
            through,
        } = drained;
        let need = OverlayNeed::of(&commands);
        let mut flushed = began;
        let mut crops = Crops::default();
        let display = self.display.map(|(frame, _, _)| frame);
        for step in steps(turn, rest) {
            match step {
                Step::PlaceRest => {
                    if self.overlay.is_some() {
                        (self.rest.place)(display, through);
                    }
                }
                Step::LiftRest => {
                    if self.overlay.is_some() {
                        (self.rest.lift)(display, through);
                    }
                }
                Step::Hide => self.overlay.as_mut().into_iter().for_each(TileOverlay::hide),
                Step::Play => {
                    CATransaction::begin();
                    CATransaction::setDisableActions(true);
                    for command in std::mem::take(&mut commands) {
                        self.apply(command);
                    }
                    CATransaction::commit();
                    CATransaction::flush();
                    flushed = Instant::now();
                    crops = self.overlay.as_mut().map(TileOverlay::take_crops).unwrap_or_default();
                }
                Step::Background => {
                    self.overlay.as_mut().into_iter().for_each(TileOverlay::paint_background)
                }
                Step::Show => self.overlay.as_mut().into_iter().for_each(TileOverlay::show),
                Step::CoverRest => {
                    if self.overlay.is_some() {
                        (self.rest.cover)(display);
                    }
                }
            }
        }
        let played = Played { turn, rest, crops };
        answer(seq, need, self.overlay_window(), played, (sent, began, flushed))
    }

    fn overlay_window(&self) -> OverlayWindow {
        match &self.overlay {
            None => OverlayWindow::Absent,
            Some(overlay) => {
                overlay.server_id().map_or(OverlayWindow::Unnumbered, OverlayWindow::Numbered)
            }
        }
    }

    fn apply(&mut self, command: Render) {
        if let Render::Display { frame, top_band, scale } = command {
            self.display = Some((frame, top_band, scale));
        }
        if needs_overlay(&command) {
            self.make_overlay();
        }
        let Some(overlay) = self.overlay.as_mut() else { return };
        match command {
            Render::Display { frame, top_band, scale } => overlay.set_frame(frame, top_band, scale),
            Render::Dress { desktop, bar, strip } => {
                overlay.set_backdrop(desktop.as_ref());
                overlay.set_bar(bar.as_ref(), strip);
            }
            Render::Stage { score, tiles } => overlay.pose(&score, &tiles),
            Render::Play { edits, tiles } => overlay.play(&edits, &tiles),
            Render::Swap { window, picture, clipped } => overlay.swap(window, &picture, clipped),
            Render::Focus { window, focused } => overlay.set_tile_focused(window, focused),
            Render::Wear { window, worn } => overlay.set_tile_worn(window, worn),
            Render::Holes(ends) => overlay.set_holes(&ends),
            // Its alpha goes up after the flush; the rest windows are placed before the transaction.
            Render::Show | Render::PlaceRest { .. } => {}
            Render::Lift { .. } => {
                overlay.clear_holes();
                overlay.release();
            }
            Render::Trace => overlay.trace(),
        }
    }

    /// The overlay over the display last laid out, unless there is one; each `Dress` or `Stage` tries
    /// again while making it fails.
    fn make_overlay(&mut self) {
        if self.overlay.is_some() {
            return;
        }
        let Some((frame, top_band, scale)) = self.display else {
            return;
        };
        self.overlay = TileOverlay::new(frame, top_band, scale, self.mtm);
        if self.overlay.is_none() {
            warn!("could not create the animation overlay; animations will be skipped");
        }
    }
}

#[cfg(test)]
mod tests {
    use rini_geometry::space::{Point, Size};

    use super::*;
    use crate::pictures::platform::picture::test_picture;

    const LIFT: Render = Render::Lift { through: None };
    const PLACE: Render = Render::PlaceRest { through: None };

    fn holes(x: f64) -> Render {
        Render::Holes(vec![(
            OverlayRect::new(Point::new(x, 32.0), Size::new(859.0, 1081.0)),
            0.0,
        )])
    }

    /// Batch `seq`, sent `ms` after `at`.
    fn batch(seq: u64, at: Instant, ms: u64, commands: Vec<Render>) -> Batch {
        Batch {
            seq,
            sent: at + std::time::Duration::from_millis(ms),
            commands,
        }
    }

    fn held_holes(commands: &[Render]) -> Vec<f64> {
        commands
            .iter()
            .filter_map(|command| match command {
                Render::Holes(ends) => Some(ends[0].0.origin.x),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn queued_batches_play_in_order_and_are_acknowledged_once() {
        let at = Instant::now();
        let drained = in_order(batch(5, at, 2, vec![holes(3.0)]), vec![
            batch(6, at, 3, vec![holes(4.0)]),
            batch(4, at, 1, vec![holes(1.0), holes(2.0)]),
        ]);
        assert_eq!(held_holes(&drained.commands), vec![1.0, 2.0, 3.0, 4.0]);
        assert_eq!(drained.seq, 6, "one answer, for the highest");
        assert_eq!(
            drained.sent,
            at + std::time::Duration::from_millis(1),
            "waiting since the oldest was sent"
        );
        assert_eq!(drained.turn, None, "neither shown nor lifted: as it was");
    }

    #[test]
    fn the_last_show_or_lift_in_a_drain_decides_whether_the_overlay_is_up() {
        let at = Instant::now();
        let shown = |woke: Batch, queued: Vec<Batch>| in_order(woke, queued).turn;
        assert_eq!(
            shown(
                batch(1, at, 0, vec![holes(1.0), Render::Show, holes(2.0)]),
                Vec::new()
            ),
            Some(OverlayTurn::Shown)
        );
        assert_eq!(
            shown(batch(1, at, 0, vec![Render::Show]), vec![batch(2, at, 1, vec![
                LIFT
            ])]),
            Some(OverlayTurn::Lifted),
            "never shown"
        );
        assert_eq!(
            shown(batch(1, at, 0, vec![LIFT, holes(1.0)]), vec![batch(
                2,
                at,
                1,
                vec![Render::Show]
            )]),
            Some(OverlayTurn::Shown)
        );
    }

    /// The show covers what is drawn at rest, so it draws nothing until it is placed, the lift draws
    /// what came since the place before the overlay hides, and a place goes ahead of everything.
    #[test]
    fn a_drain_s_steps_follow_its_turn_and_its_placement() {
        use OverlayTurn::{Lifted, Shown};
        use RestPlacement::{Placed, Unplaced};
        use Step::*;
        let table: [(Option<OverlayTurn>, RestPlacement, &[Step]); 6] = [
            (None, Unplaced, &[Play, Background]),
            (None, Placed, &[PlaceRest, Play, Background]),
            (Some(Shown), Unplaced, &[Play, Background, Show, CoverRest]),
            (Some(Shown), Placed, &[
                PlaceRest, Play, Background, Show, CoverRest,
            ]),
            (Some(Lifted), Unplaced, &[LiftRest, Hide, Play, Background]),
            (Some(Lifted), Placed, &[
                PlaceRest, LiftRest, Hide, Play, Background,
            ]),
        ];
        for (turn, rest, expected) in table {
            assert_eq!(steps(turn, rest), expected, "{turn:?} {rest:?}");
        }
    }

    /// A drain carrying `PlaceRest`, a tick ahead of the lift or in the lift's own, puts what is drawn
    /// at rest where its windows are before anything else it does.
    #[test]
    fn a_drain_carrying_place_rest_places_what_rests_before_its_commands() {
        let at = Instant::now();
        let ahead = in_order(batch(7, at, 0, vec![holes(1.0), PLACE]), Vec::new());
        assert!(ahead.rest == RestPlacement::Placed && ahead.turn.is_none());
        assert_eq!(steps(ahead.turn, ahead.rest), [
            Step::PlaceRest,
            Step::Play,
            Step::Background
        ]);
        let pressed = Some(at + std::time::Duration::from_millis(5));
        let with_the_lift =
            in_order(batch(9, at, 1, vec![Render::Lift { through: pressed }]), vec![
                batch(8, at, 0, vec![PLACE]),
            ]);
        assert_eq!(steps(with_the_lift.turn, with_the_lift.rest), [
            Step::PlaceRest,
            Step::LiftRest,
            Step::Hide,
            Step::Play,
            Step::Background
        ]);
        assert_eq!(
            with_the_lift.through, pressed,
            "both through the press the lift was given after"
        );
        assert_eq!(
            in_order(batch(1, at, 0, vec![holes(1.0)]), Vec::new()).rest,
            RestPlacement::Unplaced
        );
    }

    #[test]
    fn only_a_dress_or_a_stage_makes_the_overlay() {
        let window = WindowId::new(7, 1);
        let display = ScreenRect::new(Point::new(0.0, 0.0), Size::new(1728.0, 1117.0));
        let commands = [
            ("display", Render::Display {
                frame: display,
                top_band: 32.0,
                scale: 2.0,
            }),
            ("dress", Render::Dress {
                desktop: None,
                bar: None,
                strip: None,
            }),
            ("stage", Render::Stage {
                score: Score::default(),
                tiles: Vec::new(),
            }),
            ("play", Render::Play {
                edits: Vec::new(),
                tiles: Vec::new(),
            }),
            ("swap", Render::Swap {
                window,
                picture: test_picture(display.size),
                clipped: false,
            }),
            ("focus", Render::Focus { window, focused: true }),
            ("wear", Render::Wear { window, worn: Worn::default() }),
            ("holes", holes(1.0)),
            ("show", Render::Show),
            ("place_rest", PLACE),
            ("lift", LIFT),
            ("trace", Render::Trace),
        ];
        let making: Vec<&str> = commands
            .iter()
            .filter(|(_, command)| needs_overlay(command))
            .map(|(name, _)| *name)
            .collect();
        assert_eq!(making, ["dress", "stage"]);
    }

    /// Only a drain that needed the overlay, with none made, is not drawn; every other is committed,
    /// with the overlay's number once the window server has numbered it.
    #[test]
    fn a_drain_is_answered_by_its_need_and_the_overlay_after_it() {
        use OverlayNeed::{Needed, Unneeded};
        use OverlayWindow::{Absent, Numbered, Unnumbered};
        let at = Instant::now();
        let times = (
            at,
            at + std::time::Duration::from_millis(1),
            at + std::time::Duration::from_millis(2),
        );
        let (sent, began, flushed) = times;
        let played = Played {
            turn: Some(OverlayTurn::Shown),
            rest: RestPlacement::Placed,
            crops: Crops {
                fills: 2,
                ground: std::time::Duration::from_micros(7_910),
            },
        };
        let committed = |overlay| Event::Committed {
            seq: 3,
            overlay,
            played,
            sent,
            began,
            flushed,
        };
        let numbered = WindowServerId(4_242);
        let table = [
            (Needed, Absent, Event::Unavailable { seq: 3 }),
            (Unneeded, Absent, committed(None)),
            (Needed, Unnumbered, committed(None)),
            (Unneeded, Unnumbered, committed(None)),
            (Needed, Numbered(numbered), committed(Some(numbered))),
            (Unneeded, Numbered(numbered), committed(Some(numbered))),
        ];
        for (need, overlay, expected) in table {
            assert_eq!(
                answer(3, need, overlay, played, times),
                expected,
                "{need:?} {overlay:?}"
            );
        }
    }

    #[test]
    fn a_drain_needs_the_overlay_when_it_dresses_or_poses_a_flight() {
        assert_eq!(OverlayNeed::of(&[holes(1.0), LIFT]), OverlayNeed::Unneeded);
        let dress = Render::Dress {
            desktop: None,
            bar: None,
            strip: None,
        };
        assert_eq!(OverlayNeed::of(&[holes(1.0), dress]), OverlayNeed::Needed);
    }

    #[test]
    fn a_batch_crosses_threads() {
        fn crosses<T: Send>() {}
        crosses::<Batch>();
        crosses::<Event>();
    }

    #[test]
    fn the_media_clock_reads_what_core_animation_does() {
        let clock = media_clock();
        let before = Instant::now();
        let media = objc2_quartz_core::CACurrentMediaTime();
        let after = Instant::now();
        let (from, to) = (clock.at(before), clock.at(after));
        assert!(
            from - 0.000_1 <= media && media <= to + 0.000_1,
            "{media} outside {from}..{to}"
        );
    }
}
