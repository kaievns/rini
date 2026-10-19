//! The overview's lifecycle on the overlay, beside the flights' (`Coordinator`): a held view
//! rather than a movement, so it has no clock end and lifts only once it is closed. Frame zero, the
//! overlay up and the zoom out; held, every scene played as it comes and the pictures service let
//! go so the workspaces not shown are pictured; then the zoom back in, the real windows read where
//! they landed, the lift and the quiet after it. It writes no frame: every press inside it placed
//! the real windows as it ran. Pure: every event carries its instant, and the engine carries out
//! the commands. See `specs/overview.md`, "How it moves".

use std::time::{Duration, Instant};

use super::{FRAME_ZERO_PATIENCE, Generation, LANDING_PATIENCE};

/// What the overview's lifecycle hears.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    /// Asked to open under `generation`, zooming out and in over `length`; `flying` while a flight is
    /// up, which it waits to see lift, and opens under the generation it hears then; `rests` when
    /// anything is drawn at rest on its display.
    Open {
        generation: Generation,
        length: Duration,
        flying: bool,
        rests: bool,
    },
    /// The flight it waited for has lifted: it opens under `Generation`.
    FlightLifted(Generation),
    /// Frame zero's pictures arrived, or the service has nothing more to give.
    FrameZero(Generation),
    /// A new scene to play.
    Scene,
    Close,
    /// The frame clock: `settled` with every tile where it ends, `landed` with every window of the
    /// workspace it lands on where its tile ends.
    Tick {
        settled: bool,
        landed: bool,
    },
    /// The quiet after a lift ran out.
    Quiet(Generation),
    /// A flight began after its lift: the flight holds the service and the report from now.
    Overtaken,
    /// The renderer has no overlay to draw it on.
    Unavailable,
}

/// What the engine carries out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    /// Hold the pictures service: the overlay is going up or zooming back in.
    Hold,
    AskFrameZero(Generation),
    /// Frame zero dressed, posed and shown, and the zoom out from it begun.
    Show,
    /// Nothing holds the pictures service: zoomed out, every workspace's pictures are taken.
    Release,
    /// The latest scene played from where everything is drawn.
    Play,
    /// The zoom back in onto the real layout, from where everything is drawn, the overlay holed over
    /// the windows it lands on.
    Exit,
    /// The zoom out again from where everything is drawn, the overlay's holes filled in again:
    /// opened again as it closed.
    Turn,
    /// What is drawn at rest put where its windows are, under the overlay, a tick ahead of the lift.
    PlaceRest,
    Lift(Generation),
    /// The overlay is down: the bar is pictured again, and the rest of the service still waits.
    Settle,
    /// What it drew let go with no lift: closed before its show, or the renderer has no overlay.
    Forget,
    ReportUp,
    ReportSettled,
    /// The frame clock started, or stopped: it ticks only while something is to be waited for.
    Clock(bool),
    /// The flights held while it was up go on, in order.
    Replay,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Phase {
    /// A flight is up: it opens once that lifts.
    Waiting,
    /// Frame zero asked, the overlay not up yet.
    Preparing { deadline: Instant },
    /// Zooming out until `until`.
    Entering { until: Instant },
    /// Zoomed out. Nothing but a close ends it.
    Held,
    /// Zooming back in until `until`.
    Exiting { until: Instant },
    /// Zoomed in; waiting for the windows to read landed, until `deadline` at most, then placing
    /// what rests a tick ahead of the lift when anything does.
    Landing { deadline: Instant, placed: bool },
    /// Lifted; the quiet runs.
    Settling,
}

/// The overview's lifecycle. One per overlay, beside the flights'.
#[derive(Clone, Debug, Default)]
pub struct Overview {
    phase: Option<Phase>,
    generation: Generation,
    length: Duration,
    rests: bool,
}

impl Overview {
    /// Whether it holds the overlay, from its open to its lift: a flight asked for meanwhile waits.
    pub fn holds_overlay(&self) -> bool { self.phase.is_some_and(|phase| phase != Phase::Settling) }

    /// Whether its overlay is up and its scene drawn.
    pub fn is_shown(&self) -> bool {
        matches!(
            self.phase,
            Some(
                Phase::Entering { .. }
                    | Phase::Held
                    | Phase::Exiting { .. }
                    | Phase::Landing { .. }
            )
        )
    }

    /// Whether it has lifted and its quiet runs.
    pub fn is_settling(&self) -> bool { self.phase == Some(Phase::Settling) }

    /// Whether it waits for a flight to lift before it opens.
    pub fn is_waiting(&self) -> bool { self.phase == Some(Phase::Waiting) }

    /// Whether it is zooming back in or landing: a tick reads the windows only then.
    pub fn is_landing(&self) -> bool {
        matches!(self.phase, Some(Phase::Exiting { .. } | Phase::Landing { .. }))
    }

    /// The generation it asks its pictures and lifts under.
    pub fn generation(&self) -> Generation { self.generation }

    pub fn on(&mut self, event: Event, now: Instant) -> Vec<Command> {
        let mut out = Vec::new();
        match (self.phase, event) {
            (
                None | Some(Phase::Settling),
                Event::Open {
                    generation,
                    length,
                    flying,
                    rests,
                },
            ) => {
                self.generation = generation;
                self.length = length;
                self.rests = rests;
                if flying {
                    self.phase = Some(Phase::Waiting);
                } else {
                    self.prepare(now, &mut out);
                }
            }
            (Some(Phase::Exiting { .. } | Phase::Landing { .. }), Event::Open { .. }) => {
                out.push(Command::Turn);
                self.enter(now, &mut out);
            }
            (Some(Phase::Waiting), Event::FlightLifted(generation)) => {
                self.generation = generation;
                self.prepare(now, &mut out)
            }
            (Some(Phase::Waiting), Event::Close) => {
                self.phase = None;
                out.push(Command::Replay);
            }
            (Some(Phase::Preparing { .. }), Event::FrameZero(generation))
                if generation == self.generation =>
            {
                out.push(Command::Show);
                self.enter(now, &mut out);
            }
            (Some(Phase::Preparing { deadline }), Event::Tick { .. }) if now >= deadline => {
                out.push(Command::Show);
                self.enter(now, &mut out);
            }
            (Some(Phase::Preparing { .. }), Event::Close | Event::Unavailable)
            | (Some(Phase::Entering { .. } | Phase::Held), Event::Unavailable) => {
                self.phase = None;
                out.extend([
                    Command::Forget,
                    Command::Clock(false),
                    Command::Release,
                    Command::ReportSettled,
                    Command::Replay,
                ]);
            }
            (Some(Phase::Entering { until }), Event::Tick { .. }) if now >= until => {
                self.hold(&mut out);
            }
            (Some(Phase::Entering { .. } | Phase::Held), Event::Scene) => out.push(Command::Play),
            (Some(Phase::Entering { .. } | Phase::Held), Event::Close) => {
                out.extend([Command::Hold, Command::Exit, Command::Clock(true)]);
                self.phase = Some(Phase::Exiting { until: now + self.length });
            }
            (Some(Phase::Exiting { until }), Event::Tick { settled, landed }) if now >= until => {
                self.phase = Some(Phase::Landing {
                    deadline: now + LANDING_PATIENCE,
                    placed: false,
                });
                self.land(settled && landed, now, &mut out);
            }
            (Some(Phase::Landing { .. }), Event::Tick { settled, landed }) => {
                self.land(settled && landed, now, &mut out)
            }
            (Some(Phase::Exiting { .. } | Phase::Landing { .. }), Event::Unavailable) => {
                self.lift(&mut out)
            }
            (Some(Phase::Settling), Event::Quiet(generation)) if generation == self.generation => {
                self.phase = None;
                out.extend([Command::Release, Command::ReportSettled]);
            }
            (Some(Phase::Settling), Event::Overtaken) => self.phase = None,
            _ => {}
        }
        out
    }

    fn prepare(&mut self, now: Instant, out: &mut Vec<Command>) {
        out.extend([
            Command::Hold,
            Command::AskFrameZero(self.generation),
            Command::ReportUp,
            Command::Clock(true),
        ]);
        self.phase = Some(Phase::Preparing {
            deadline: now + FRAME_ZERO_PATIENCE,
        });
    }

    /// Zooming out until its length runs out, held at once with none.
    fn enter(&mut self, now: Instant, out: &mut Vec<Command>) {
        if self.length.is_zero() {
            self.hold(out);
        } else {
            self.phase = Some(Phase::Entering { until: now + self.length });
        }
    }

    fn hold(&mut self, out: &mut Vec<Command>) {
        self.phase = Some(Phase::Held);
        out.extend([Command::Release, Command::Clock(false)]);
    }

    /// Once its windows read landed, or its patience runs out: what rests placed first when
    /// anything does, and the lift a tick after.
    fn land(&mut self, landed: bool, now: Instant, out: &mut Vec<Command>) {
        let Some(Phase::Landing { deadline, placed }) = self.phase else {
            return;
        };
        if !landed && now < deadline {
            return;
        }
        if self.rests && !placed {
            out.push(Command::PlaceRest);
            self.phase = Some(Phase::Landing { deadline, placed: true });
            return;
        }
        self.lift(out);
    }

    fn lift(&mut self, out: &mut Vec<Command>) {
        self.phase = Some(Phase::Settling);
        out.extend([
            Command::Lift(self.generation),
            Command::Settle,
            Command::Clock(false),
            Command::Replay,
        ]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LENGTH: Duration = Duration::from_millis(350);
    const TICK: Duration = Duration::from_micros(16_667);

    fn opened(flying: bool, at: Instant) -> (Overview, Vec<Command>) {
        let mut overview = Overview::default();
        let out = overview.on(
            Event::Open {
                generation: Generation(4),
                length: LENGTH,
                flying,
                rests: true,
            },
            at,
        );
        (overview, out)
    }

    /// Opened, framed and zoomed out: held from the end of its entry on.
    fn held(at: Instant) -> Overview {
        let (mut overview, _) = opened(false, at);
        overview.on(Event::FrameZero(Generation(4)), at);
        overview.on(Event::Tick { settled: true, landed: true }, at + LENGTH);
        assert!(overview.is_shown() && !overview.is_landing());
        overview
    }

    fn tick(overview: &mut Overview, at: Instant) -> Vec<Command> {
        overview.on(Event::Tick { settled: true, landed: true }, at)
    }

    /// Opened, it holds the service and asks frame zero; shown, it zooms out; held once its entry
    /// has played, it lets the service go, stops its clock and never lifts, however long it is held
    /// and whatever the clock says.
    #[test]
    fn an_overview_holds_after_its_entry_and_never_lifts_while_held() {
        let t0 = Instant::now();
        let (mut overview, out) = opened(false, t0);
        assert_eq!(out, vec![
            Command::Hold,
            Command::AskFrameZero(Generation(4)),
            Command::ReportUp,
            Command::Clock(true),
        ]);
        assert!(overview.holds_overlay() && !overview.is_shown());
        assert_eq!(overview.on(Event::FrameZero(Generation(4)), t0), vec![
            Command::Show
        ]);
        assert!(
            tick(&mut overview, t0 + LENGTH / 2).is_empty(),
            "still zooming out"
        );
        assert_eq!(tick(&mut overview, t0 + LENGTH), vec![
            Command::Release,
            Command::Clock(false)
        ]);
        for later in [1, 5, 60] {
            let at = t0 + Duration::from_secs(later);
            assert!(tick(&mut overview, at).is_empty(), "{later}s in");
            assert!(overview.on(Event::Quiet(Generation(4)), at).is_empty());
        }
        assert!(overview.is_shown() && overview.holds_overlay());
    }

    /// Frame zero that does not come shows the overlay at its patience, as a flight's does.
    #[test]
    fn frame_zero_that_does_not_come_shows_it_at_the_patience() {
        let t0 = Instant::now();
        let (mut overview, _) = opened(false, t0);
        assert!(tick(&mut overview, t0 + FRAME_ZERO_PATIENCE / 2).is_empty());
        assert!(
            overview.on(Event::FrameZero(Generation(3)), t0).is_empty(),
            "another's"
        );
        assert_eq!(tick(&mut overview, t0 + FRAME_ZERO_PATIENCE), vec![
            Command::Show
        ]);
    }

    /// A scene while it zooms out or while it is held is played as it comes, and it holds on.
    #[test]
    fn a_scene_while_held_is_played_as_it_comes_and_it_holds_on() {
        let t0 = Instant::now();
        let (mut overview, _) = opened(false, t0);
        overview.on(Event::FrameZero(Generation(4)), t0);
        assert_eq!(overview.on(Event::Scene, t0 + LENGTH / 3), vec![Command::Play]);
        tick(&mut overview, t0 + LENGTH);
        assert_eq!(overview.on(Event::Scene, t0 + LENGTH * 2), vec![Command::Play]);
        assert!(overview.is_shown() && !overview.is_landing());
    }

    /// Closed, it holds the service and zooms back in; once the zoom has played and its windows
    /// read landed it places what rests, lifts a tick later, and the quiet after it lets the
    /// service go and reports it settled, the flights held meanwhile going on at the lift.
    #[test]
    fn close_plays_the_exit_and_lifts_once_frames_read_landed() {
        let t0 = Instant::now();
        let mut overview = held(t0);
        let closed = t0 + Duration::from_secs(2);
        assert_eq!(overview.on(Event::Close, closed), vec![
            Command::Hold,
            Command::Exit,
            Command::Clock(true)
        ]);
        assert!(overview.is_landing());
        assert!(tick(&mut overview, closed + LENGTH / 2).is_empty());
        let unlanded = Event::Tick { settled: true, landed: false };
        assert!(overview.on(unlanded, closed + LENGTH).is_empty(), "not landed");
        assert!(overview.on(unlanded, closed + LENGTH + TICK).is_empty());
        let landed = closed + LENGTH + 2 * TICK;
        assert_eq!(tick(&mut overview, landed), vec![Command::PlaceRest]);
        assert_eq!(tick(&mut overview, landed + TICK), vec![
            Command::Lift(Generation(4)),
            Command::Settle,
            Command::Clock(false),
            Command::Replay,
        ]);
        assert!(!overview.holds_overlay());
        assert!(overview.on(Event::Quiet(Generation(3)), landed).is_empty());
        assert_eq!(overview.on(Event::Quiet(Generation(4)), landed), vec![
            Command::Release,
            Command::ReportSettled
        ]);
        assert!(overview.on(Event::Scene, landed).is_empty());
    }

    /// Windows that never read landed are lifted over at the landing's patience.
    #[test]
    fn windows_that_never_land_are_lifted_over_at_the_patience() {
        let t0 = Instant::now();
        let mut overview = held(t0);
        overview.on(Event::Close, t0);
        let unlanded = Event::Tick { settled: true, landed: false };
        overview.on(unlanded, t0 + LENGTH);
        let out = overview.on(unlanded, t0 + LENGTH + LANDING_PATIENCE);
        assert_eq!(out, vec![Command::PlaceRest]);
        assert_eq!(
            overview.on(unlanded, t0 + LENGTH + LANDING_PATIENCE + TICK)[0],
            Command::Lift(Generation(4))
        );
    }

    /// A release while it still zooms out turns it back there and then: no entry to finish first.
    #[test]
    fn a_release_during_the_entry_turns_back_at_once() {
        let t0 = Instant::now();
        let (mut overview, _) = opened(false, t0);
        overview.on(Event::FrameZero(Generation(4)), t0);
        let out = overview.on(Event::Close, t0 + LENGTH / 3);
        assert_eq!(out, vec![Command::Hold, Command::Exit, Command::Clock(true)]);
        assert!(
            tick(&mut overview, t0 + LENGTH).is_empty(),
            "the zoom in runs from the release"
        );
        assert_eq!(tick(&mut overview, t0 + LENGTH / 3 + LENGTH), vec![
            Command::PlaceRest
        ]);
    }

    /// Opened again while it zooms back in, it zooms out again from there, and holds once that
    /// has played.
    #[test]
    fn opened_again_while_it_closes_it_zooms_out_again() {
        let t0 = Instant::now();
        let mut overview = held(t0);
        overview.on(Event::Close, t0);
        let again = Event::Open {
            generation: Generation(5),
            length: LENGTH,
            flying: false,
            rests: true,
        };
        assert_eq!(overview.on(again, t0 + LENGTH / 2), vec![Command::Turn]);
        assert_eq!(
            overview.generation(),
            Generation(4),
            "the same overlay, the same generation"
        );
        assert_eq!(tick(&mut overview, t0 + LENGTH / 2 + LENGTH), vec![
            Command::Release,
            Command::Clock(false)
        ]);
        assert!(overview.is_shown() && !overview.is_landing());
    }

    /// Asked to open while a flight is up, it waits for that flight's lift; closed before then, it
    /// was never anything, and what waited with it goes on.
    #[test]
    fn opening_while_a_flight_flies_waits_for_its_lift() {
        let t0 = Instant::now();
        let (mut overview, out) = opened(true, t0);
        assert!(out.is_empty());
        assert!(overview.is_waiting() && overview.holds_overlay());
        let lifted = overview.on(Event::FlightLifted(Generation(6)), t0);
        assert_eq!(lifted[1], Command::AskFrameZero(Generation(6)));

        let (mut overview, _) = opened(true, t0);
        assert_eq!(
            overview.on(Event::Close, t0),
            vec![Command::Replay],
            "what waited goes on"
        );
        assert!(!overview.holds_overlay());
    }

    /// Closed before its overlay went up, it shows nothing and gives back what it held.
    #[test]
    fn a_close_before_the_show_shows_nothing() {
        let t0 = Instant::now();
        let (mut overview, _) = opened(false, t0);
        assert_eq!(overview.on(Event::Close, t0), vec![
            Command::Forget,
            Command::Clock(false),
            Command::Release,
            Command::ReportSettled,
            Command::Replay,
        ]);
        assert!(!overview.holds_overlay());
        assert!(overview.on(Event::FrameZero(Generation(4)), t0).is_empty());
    }

    /// With no overlay to draw on as it zooms out or while it is held, it lets go of what it drew,
    /// with no lift, so the next open draws afresh.
    #[test]
    fn with_no_overlay_while_shown_it_forgets_what_it_drew() {
        let t0 = Instant::now();
        let (mut overview, _) = opened(false, t0);
        overview.on(Event::FrameZero(Generation(4)), t0);
        assert_eq!(overview.on(Event::Unavailable, t0)[0], Command::Forget);
        assert!(!overview.holds_overlay());
        let mut overview = held(t0);
        assert_eq!(overview.on(Event::Unavailable, t0 + LENGTH), vec![
            Command::Forget,
            Command::Clock(false),
            Command::Release,
            Command::ReportSettled,
            Command::Replay,
        ]);
        assert!(!overview.holds_overlay());
    }

    /// With no length to play over, animation off or Low Power Mode, it is held as it shows and lifts as
    /// soon as its windows have landed: cuts both ways.
    #[test]
    fn with_no_length_it_opens_and_closes_as_cuts() {
        let t0 = Instant::now();
        let mut overview = Overview::default();
        overview.on(
            Event::Open {
                generation: Generation(1),
                length: Duration::ZERO,
                flying: false,
                rests: false,
            },
            t0,
        );
        assert_eq!(overview.on(Event::FrameZero(Generation(1)), t0), vec![
            Command::Show,
            Command::Release,
            Command::Clock(false)
        ]);
        overview.on(Event::Close, t0);
        assert_eq!(tick(&mut overview, t0)[0], Command::Lift(Generation(1)));
    }

    /// A flight beginning in its quiet takes the service and the report: the quiet lets nothing go.
    #[test]
    fn a_flight_in_its_quiet_takes_the_service_from_it() {
        let t0 = Instant::now();
        let mut overview = held(t0);
        overview.on(Event::Close, t0);
        overview.on(Event::Tick { settled: true, landed: true }, t0 + LENGTH);
        overview.on(Event::Tick { settled: true, landed: true }, t0 + LENGTH + TICK);
        overview.on(Event::Overtaken, t0 + LENGTH + TICK);
        assert!(overview.on(Event::Quiet(Generation(4)), t0 + LENGTH * 3).is_empty());
    }
}
