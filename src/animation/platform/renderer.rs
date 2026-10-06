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
use crate::animation::event::Event;
use crate::animation::platform::overlay::{OverlayTile, TileOverlay};
use crate::pictures::platform::picture::Picture;

/// Everything the engine asks of the overlay, each one of its calls. Nothing asks for anything back.
pub enum Render {
    /// The display the overlay covers, in Screen space, the band the bar reserves at its top and the
    /// scale.
    Display { frame: ScreenRect, top_band: f64, scale: f64 },
    /// The desktop drawn behind the tiles and the bar over them, at `strip`.
    Dress { desktop: Option<Picture>, bar: Option<Picture>, strip: Option<OverlayRect> },
    /// Frame zero, and each pose of it again before the motion, every tile dressed from `tiles`.
    Stage { score: Score, tiles: Vec<OverlayTile> },
    /// The score's edits: the motion, merges, accents, snaps and banks; a tile first ridden is
    /// dressed from `tiles`.
    Play { edits: Vec<Edit>, tiles: Vec<OverlayTile> },
    /// A fresh picture onto `window`'s tile, its movement left alone.
    Swap { window: WindowId, picture: Picture },
    /// `window`'s picture clipped to its corners at `radius`, or drawn whole.
    Corner { window: WindowId, radius: Option<f64> },
    /// `window`'s tile drawn focused or not.
    Focus { window: WindowId, focused: bool },
    /// The overlay holed over each window ending at one of these, the holes before replaced.
    Holes(Vec<OverlayRect>),
    /// The overlay up.
    Show,
    /// The overlay down, its holes filled, and its tiles and the desktop behind them let go.
    Lift,
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
/// was sent, and whether the overlay ends the drain up (`Some(true)`), down (`Some(false)`) or as it
/// was.
pub struct Drained {
    pub commands: Vec<Render>,
    pub seq: u64,
    pub sent: Instant,
    pub shown: Option<bool>,
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
    let shown = commands.iter().rev().find_map(|command| match command {
        Render::Show => Some(true),
        Render::Lift => Some(false),
        _ => None,
    });
    Drained { commands, seq, sent, shown }
}

/// Whether `command` makes the overlay when there is none: it dresses or poses a flight.
fn needs_overlay(command: &Render) -> bool { matches!(command, Render::Dress { .. } | Render::Stage { .. }) }

/// What a drain comes to, given the overlay after it and its window's number: not drawn when it
/// `needed` the overlay and there is none, else committed, any drain with no overlay included, with
/// the window's number once the window server has numbered it.
fn answer(
    seq: u64,
    needed: bool,
    overlay: Option<Option<WindowServerId>>,
    shown: Option<bool>,
    (sent, began, flushed): (Instant, Instant, Instant),
) -> Event {
    match overlay {
        None if needed => Event::Unavailable { seq },
        overlay => Event::Committed { seq, overlay: overlay.flatten(), shown, sent, began, flushed },
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
    mtm: MainThreadMarker,
}

impl Renderer {
    pub fn new(rx: Receiver<Batch>, report: Box<dyn Fn(Event)>, mtm: MainThreadMarker) -> Self {
        Self { rx, overlay: None, display: None, report, mtm }
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

    /// Plays a drain in one transaction, flushed: the overlay brought down before it when the drain
    /// lifts it, put up after when the drain shows it, and the window's background set once the
    /// holes' mask is committed. Without an overlay every command but `Display` does nothing; a drain
    /// that dressed or posed a flight on none is `Unavailable`. A commit says when the drain began
    /// and when it was flushed.
    fn play(&mut self, drained: Drained) -> Event {
        let began = Instant::now();
        let Drained { commands, seq, sent, shown } = drained;
        let needed = commands.iter().any(needs_overlay);
        if shown == Some(false)
            && let Some(overlay) = self.overlay.as_mut()
        {
            overlay.hide();
        }
        CATransaction::begin();
        CATransaction::setDisableActions(true);
        for command in commands {
            self.apply(command);
        }
        CATransaction::commit();
        CATransaction::flush();
        let flushed = Instant::now();
        if let Some(overlay) = self.overlay.as_mut() {
            overlay.paint_background();
            if shown == Some(true) {
                overlay.show();
            }
        }
        let overlay = self.overlay.as_ref().map(TileOverlay::server_id);
        answer(seq, needed, overlay, shown, (sent, began, flushed))
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
            Render::Swap { window, picture } => overlay.swap(window, &picture),
            Render::Corner { window, radius } => overlay.set_tile_corner_clip(window, radius),
            Render::Focus { window, focused } => overlay.set_tile_focused(window, focused),
            Render::Holes(ends) => overlay.set_holes(&ends),
            // Its alpha goes up after the flush.
            Render::Show => {}
            Render::Lift => {
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
        let Some((frame, top_band, scale)) = self.display else { return };
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

    fn holes(x: f64) -> Render {
        Render::Holes(vec![OverlayRect::new(Point::new(x, 32.0), Size::new(859.0, 1081.0))])
    }

    /// Batch `seq`, sent `ms` after `at`.
    fn batch(seq: u64, at: Instant, ms: u64, commands: Vec<Render>) -> Batch {
        Batch { seq, sent: at + std::time::Duration::from_millis(ms), commands }
    }

    fn held_holes(commands: &[Render]) -> Vec<f64> {
        commands
            .iter()
            .filter_map(|command| match command {
                Render::Holes(ends) => Some(ends[0].origin.x),
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
        assert_eq!(drained.sent, at + std::time::Duration::from_millis(1), "waiting since the oldest was sent");
        assert_eq!(drained.shown, None, "neither shown nor lifted: as it was");
    }

    #[test]
    fn the_last_show_or_lift_in_a_drain_decides_whether_the_overlay_is_up() {
        let at = Instant::now();
        let shown = |woke: Batch, queued: Vec<Batch>| in_order(woke, queued).shown;
        assert_eq!(shown(batch(1, at, 0, vec![holes(1.0), Render::Show, holes(2.0)]), Vec::new()), Some(true));
        assert_eq!(shown(batch(1, at, 0, vec![Render::Show]), vec![batch(2, at, 1, vec![Render::Lift])]), Some(false), "never shown");
        assert_eq!(shown(batch(1, at, 0, vec![Render::Lift, holes(1.0)]), vec![batch(2, at, 1, vec![Render::Show])]), Some(true));
    }

    #[test]
    fn only_a_dress_or_a_stage_makes_the_overlay() {
        let window = WindowId::new(7, 1);
        let display = ScreenRect::new(Point::new(0.0, 0.0), Size::new(1728.0, 1117.0));
        let commands = [
            ("display", Render::Display { frame: display, top_band: 32.0, scale: 2.0 }),
            ("dress", Render::Dress { desktop: None, bar: None, strip: None }),
            ("stage", Render::Stage { score: Score::default(), tiles: Vec::new() }),
            ("play", Render::Play { edits: Vec::new(), tiles: Vec::new() }),
            ("swap", Render::Swap { window, picture: test_picture(display.size) }),
            ("corner", Render::Corner { window, radius: None }),
            ("focus", Render::Focus { window, focused: true }),
            ("holes", holes(1.0)),
            ("show", Render::Show),
            ("lift", Render::Lift),
            ("trace", Render::Trace),
        ];
        let making: Vec<&str> =
            commands.iter().filter(|(_, command)| needs_overlay(command)).map(|(name, _)| *name).collect();
        assert_eq!(making, ["dress", "stage"]);
    }

    #[test]
    fn a_drain_that_needed_the_overlay_and_has_none_is_not_drawn() {
        let at = Instant::now();
        assert_eq!(answer(3, true, None, Some(true), (at, at, at)), Event::Unavailable { seq: 3 });
    }

    #[test]
    fn every_other_drain_is_committed_with_the_windows_number_once_it_has_one() {
        let at = Instant::now();
        let times = (at, at + std::time::Duration::from_millis(1), at + std::time::Duration::from_millis(2));
        let (sent, began, flushed) = times;
        let committed = |overlay| Event::Committed { seq: 3, overlay, shown: None, sent, began, flushed };
        let numbered = WindowServerId(4_242);
        assert_eq!(answer(3, false, None, None, times), committed(None), "a display or holes, no overlay");
        assert_eq!(answer(3, true, Some(None), None, times), committed(None), "made, not numbered yet");
        assert_eq!(answer(3, true, Some(Some(numbered)), None, times), committed(Some(numbered)));
        assert_eq!(answer(3, false, Some(Some(numbered)), None, times), committed(Some(numbered)), "every drain");
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
        assert!(from - 0.000_1 <= media && media <= to + 0.000_1, "{media} outside {from}..{to}");
    }
}
