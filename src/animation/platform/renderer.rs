//! The renderer: the main thread's actor that owns the tile overlay and plays the render commands the
//! flight engine sends it, a batch an event. The batches queued when it wakes are one drain, played
//! in one `CATransaction`, flushed so its answer means the render server has it; the window's alpha
//! and background, which are not the layers', are set around it. It decides nothing: every motion it
//! plays is an edit of the coordinator's score, begun on the media clock `media_clock` reads. See
//! "One transaction a drain, flushed" in `src/animation/docs/README.md`.

use std::time::Instant;

use objc2::MainThreadMarker;
use objc2_quartz_core::CATransaction;
use rini_core::ids::WindowId;
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
    /// The overlay down, its holes filled and its tiles let go.
    Lift,
    /// Where each container is presented, logged; sent only while `rini::animation::trace` is on.
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

/// Core Animation's media clock against `Instant`, read now: what the engine begins every track on.
pub fn media_clock() -> MediaClock {
    MediaClock::anchored(Instant::now(), objc2_quartz_core::CACurrentMediaTime())
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
        let draws_a_flight =
            commands.iter().any(|command| matches!(command, Render::Dress { .. } | Render::Stage { .. }));
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
        let Some(overlay) = self.overlay.as_mut() else {
            return if draws_a_flight {
                Event::Unavailable { seq }
            } else {
                Event::Committed { seq, overlay: None, shown, sent, began, flushed }
            };
        };
        overlay.paint_background();
        if shown == Some(true) {
            overlay.show();
        }
        Event::Committed { seq, overlay: overlay.server_id(), shown, sent, began, flushed }
    }

    fn apply(&mut self, command: Render) {
        if let Render::Display { frame, top_band, scale } = command {
            self.display = Some((frame, top_band, scale));
        }
        if matches!(command, Render::Dress { .. } | Render::Stage { .. }) {
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
                overlay.release_tiles();
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
    fn a_batch_crosses_threads() {
        fn crosses<T: Send>() {}
        crosses::<Batch>();
        crosses::<Event>();
    }

    #[test]
    fn the_media_clock_reads_what_core_animation_does() {
        let clock = media_clock();
        let (at, media) = (clock.at(Instant::now()), objc2_quartz_core::CACurrentMediaTime());
        assert!((at - media).abs() < 0.000_1, "{at} against {media}");
    }
}
