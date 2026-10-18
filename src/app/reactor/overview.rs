//! The overview's session on the reactor's side: opened and closed by the tap's hold
//! (`input::domain::switch_session`), its scene laid out from the real layout as it opens and after
//! every batch while it is open and handed to the flight engine, which draws it, every flight a
//! press inside it asks for held back with its frames placed at once and its press handed on with
//! the scene, and the mouse kept from moving focus or the pointer until it closes
//! (`specs/overview.md`).

use objc2_core_foundation::{CGPoint, CGRect};
use rini_core::ids::{SpaceId, WindowId};
use rini_geometry::space::{FromCg, ScreenRect, ToCg};
use rustc_hash::{FxHashMap as HashMap, FxHashSet as HashSet};
use tracing::{debug, info};

use super::events::EventOutcome;
use super::{Reactor, ScreenInfo};
use crate::animation::domain::choreography::Cue;
use crate::animation::domain::choreography::overview::Press;
use crate::animation::domain::choreography::zoom::ZOOM_STRETCH;
use crate::input::domain::switch_session::{OverviewSignal, SessionId};
use crate::overview::domain::scene::{ColumnInput, Holder, Laid, OverviewInput, RowInput, lay_out};
use crate::transitions::domain::request::{OverviewView, OverviewWindow};
use crate::transitions::platform::engine::Event as Flight;
use crate::workspaces::VirtualWorkspaceId;

/// The overview, while it is open, and the windows its presses placed that a flight still on its way
/// would put back.
#[derive(Default)]
pub(super) struct OverviewFeed {
    pub(super) live: Option<LiveOverview>,
    /// Windows a press inside the overview placed since the flight engine was last sent a flight:
    /// a flight sent before it opened that lands now would put them back where it was taking them.
    superseded: HashSet<WindowId>,
}

/// An overview in progress.
pub(super) struct LiveOverview {
    /// The tap's session it is, which closing it from here ends.
    session: SessionId,
    /// The space of the display it shows.
    space: SpaceId,
    /// The scene as last laid out, with what the engine draws it from.
    view: OverviewView,
    /// The presses held flights named since the scene was last laid out, in order: a batch can hold
    /// several, a step and then a push against the end among them.
    pending: Vec<Press>,
    /// The presses the scene last laid out answers, none where no press changed it.
    presses: Vec<Press>,
    /// Where the pointer goes as it closes: the last warp asked for while it was open.
    warp: Option<Warp>,
}

/// A warp asked for while the overview was open.
#[derive(Clone, Copy, Debug)]
pub(super) enum Warp {
    To(CGPoint),
    /// The centre of the window focus went to, read as the overview closes, once it has landed.
    Onto(WindowId),
}

impl Reactor {
    /// Handle the overview's signal from the input thread. Neither changes focus: every press
    /// inside it runs as it does outside, and its close undoes none of them.
    pub(super) fn handle_overview_signal(&mut self, signal: OverviewSignal) -> EventOutcome {
        info!(?signal, "overview");
        match signal {
            OverviewSignal::Open(session) => self.open_overview(session),
            OverviewSignal::Close => self.close_overview(),
        }
        EventOutcome::no_change()
    }

    /// Opens the overview as the tap's `session` on the focused display, hands the flight engine its
    /// scene to draw over that display, the rest windows told a flight began there, and warms every
    /// workspace of it so its thumbs have pictures. Refused, and that session ended, where that
    /// display shows a space rini does not manage, a native full-screen one included. One open as
    /// another session is closed first: the tap holds one session at a time, and the newest is the
    /// one it holds.
    fn open_overview(&mut self, session: SessionId) {
        if let Some(live) = &self.overview.live {
            if live.session == session {
                return;
            }
            self.close_overview();
        }
        let view = self
            .overview_display()
            .and_then(|space| Some((space, self.lay_out_overview(space, 0.0)?)));
        let Some((space, view)) = view else {
            info!("overview refused: the focused display shows no space rini manages");
            self.end_tap_session(session);
            return;
        };
        let display = self.publish_animation_display_for(Some(space));
        let cause = self.cause;
        self.rest.flight = display.map(|display| (display, cause.at)).or(self.rest.flight);
        let (arc, press) = self.overview_motion();
        self.tell_overview(Flight::OverviewOpen {
            view: view.clone(),
            arc,
            press,
            cause,
        });
        self.overview.live = Some(LiveOverview {
            session,
            space,
            view,
            pending: Vec::new(),
            presses: Vec::new(),
            warp: None,
        });
        self.warm_all_workspaces(space);
    }

    /// How long the overview zooms out and in, and how long a press inside it plays: the zoom
    /// look's stretch of the configured length, and that length, both none with animation off or in
    /// Low Power Mode, which the engine draws as cuts.
    fn overview_motion(&self) -> (std::time::Duration, std::time::Duration) {
        if !self.config.settings.animate || (self.reads.low_power)() {
            return Default::default();
        }
        let press = self.config.settings.animation_duration.max(0.0);
        (
            std::time::Duration::from_secs_f64(press * ZOOM_STRETCH),
            std::time::Duration::from_secs_f64(press),
        )
    }

    /// Hands the flight engine what the overview draws.
    fn tell_overview(&self, event: Flight) {
        if let Some(tx) = &self.communication_manager.transitions_tx {
            tx.send(event);
        }
    }

    /// Closes the overview, the engine zooming back in onto the real layout as it is now, and
    /// carries out the last warp asked for while it was open. Focus is where its last press left
    /// it, which is what the scene selected.
    fn close_overview(&mut self) {
        let Some(live) = self.overview.live.take() else {
            return;
        };
        let view = self.lay_out_overview(live.space, live.view.scene.scroll).unwrap_or(live.view);
        self.tell_overview(Flight::OverviewClose { view });
        let point = match live.warp {
            Some(Warp::To(point)) => Some(point),
            Some(Warp::Onto(window)) => self.affinity().window_center_on_known_screen(window),
            None => None,
        };
        if let Some(point) = point {
            self.warp_mouse(point);
        }
    }

    /// Lays the open overview out again from what the batch did, keeping the scene only when it
    /// changed or a press named a change; closes it, and ends the tap's session, once focus has
    /// left its display or the display is gone. Once per batch of events.
    pub(super) fn relay_overview(&mut self) {
        let Some(live) = &self.overview.live else { return };
        let (session, space, scroll) = (live.session, live.space, live.view.scene.scroll);
        let view = if self.focus_left_overview(space) {
            None
        } else {
            self.lay_out_overview(space, scroll)
        };
        let Some(view) = view else {
            info!("overview closed: focus left its display, or the display is gone");
            self.close_overview();
            self.end_tap_session(session);
            return;
        };
        let Some(live) = self.overview.live.as_mut() else {
            return;
        };
        if view == live.view && live.pending.is_empty() {
            return;
        }
        live.presses = std::mem::take(&mut live.pending);
        debug!(selected = ?view.scene.selected, presses = ?live.presses, "overview laid out again");
        live.view = view.clone();
        let presses = live.presses.clone();
        self.tell_overview(Flight::OverviewScene { view, presses });
    }

    /// `flight`, over the display holding `space`, held back while the overview is open: the real
    /// windows placed where it would have placed them once it covered them, and its press noted for
    /// the scene when it is the overview's display's. Handed back when the overview is not open, or
    /// when it is no flight; then a flight sent to the engine supersedes whatever a press inside the
    /// overview placed.
    pub(super) fn hold_for_overview(&mut self, space: SpaceId, flight: Flight) -> Option<Flight> {
        let Some(live) = self.overview.live.as_mut() else {
            if flight.pressed().is_some() {
                self.overview.superseded.clear();
            }
            return Some(flight);
        };
        let (press, frames) = match flight {
            Flight::Animate { windows, .. } => (
                Press::Moved(Cue::Plain),
                windows.iter().map(|request| (request.window, request.to)).collect(),
            ),
            Flight::AnimateSurface { cue, final_frames, .. } => (Press::Moved(cue), final_frames),
            Flight::Bounce { cue, overshoot, .. } => {
                (Press::Pushed { press: cue, overshoot }, Vec::new())
            }
            other => return Some(other),
        };
        if live.space == space {
            live.pending.push(press);
        }
        let frames: Vec<(WindowId, CGRect)> =
            frames.into_iter().map(|(window, frame)| (window, frame.to_cg())).collect();
        self.overview.superseded.extend(frames.iter().map(|(window, _)| *window));
        self.place_frames_on(Some(space), frames);
        None
    }

    /// `frames`, a flight's, less those of windows a press inside the overview has placed since.
    pub(super) fn not_superseded(
        &self,
        mut frames: Vec<(WindowId, CGRect)>,
    ) -> Vec<(WindowId, CGRect)> {
        frames.retain(|(window, _)| !self.overview.superseded.contains(window));
        frames
    }

    /// Holds `warp` for the overview's close while it is open; whether it did.
    pub(super) fn hold_warp(&mut self, warp: Warp) -> bool {
        let Some(live) = self.overview.live.as_mut() else {
            return false;
        };
        live.warp = Some(warp);
        true
    }

    /// Whether the overview is open, the windows under the pointer hidden behind it.
    pub(super) fn overview_covers_the_pointer(&self) -> bool { self.overview.live.is_some() }

    /// Tells the tap its `session` is over from here, so it stops holding keys for it.
    pub(super) fn end_tap_session(&self, session: SessionId) {
        if let Some(event_tap_tx) = &self.communication_manager.event_tap_tx {
            event_tap_tx.send(crate::input::platform::input_tap::Request::EndSwitchSession(
                session,
            ));
        }
    }

    /// The space of the display the user is on: where the layout's focus is, else the key window,
    /// else the space commands act on. The layout's focus first, as `focus_left_overview` reads it,
    /// or a key window the layout does not track on another display opens it where it then closes.
    fn overview_display(&self) -> Option<SpaceId> {
        self.layout_engine
            .focused_window()
            .or_else(|| self.main_window())
            .and_then(|window| self.space_of(window))
            .or_else(|| self.raw_command_space())
    }

    /// Whether the layout's focus is on a window of another display than `space`'s.
    fn focus_left_overview(&self, space: SpaceId) -> bool {
        self.layout_engine
            .focused_window()
            .and_then(|window| self.space_of(window))
            .is_some_and(|focused| focused != space)
    }

    fn space_of(&self, window: WindowId) -> Option<SpaceId> {
        let affinity = self.affinity();
        affinity
            .assigned_space_for_window_id(window)
            .or_else(|| affinity.best_space_for_window_id(window))
    }

    /// The scene `space`'s display is laid out as from the layout now, its stack scrolled by
    /// `scroll` as it was last laid out, with what the engine draws it from; none where it shows no
    /// space rini manages.
    fn lay_out_overview(&mut self, space: SpaceId, scroll: f64) -> Option<OverviewView> {
        if !self.is_space_active(space) {
            return None;
        }
        let input = self.overview_input(space, scroll)?;
        let scene = lay_out(&input);
        let shown = input.rows.get(input.active).map_or_else(Vec::new, |row| {
            let tiled = row.columns.iter().flat_map(|column| &column.windows);
            tiled.chain(&row.floating).map(|laid| (laid.window, laid.frame)).collect()
        });
        let rings = self.layout_rings();
        let windows = scene
            .rows
            .iter()
            .flat_map(|row| &row.tiles)
            .filter_map(|tile| {
                let server_id = self.state.windows.window(tile.window)?.info.sys_id?;
                Some(OverviewWindow {
                    window: tile.window,
                    server_id,
                    decoration: self.decoration_of(tile.window, &rings),
                    floating: tile.holder == Holder::Floating,
                })
            })
            .collect();
        let outline = crate::decorations::domain::rules::decoration(
            &self.config.settings.decorations,
            Default::default(),
        )
        .ring;
        Some(OverviewView { scene, windows, shown, outline })
    }

    /// What `space`'s scene is laid out from: a row per workspace in the stack's order, each its
    /// strip's columns and floating windows where the layout has them, aligned on the window focus
    /// is on, else on the one a switch to the workspace focuses.
    fn overview_input(&mut self, space: SpaceId, scroll: f64) -> Option<OverviewInput> {
        let screen = self.space_state.screen_by_space(space)?.clone();
        let gaps = self
            .config
            .settings
            .layout
            .gaps
            .effective_for_display(screen.display_uuid_opt());
        let workspaces = self.layout_engine.virtual_workspace_manager_mut().list_workspaces(space);
        let shown = self.layout_engine.active_workspace(space)?;
        let active = workspaces.iter().position(|(workspace, _)| *workspace == shown)?;
        let rows = workspaces
            .iter()
            .map(|(workspace, _)| self.overview_row(space, *workspace, &screen, &gaps))
            .collect();
        Some(OverviewInput {
            rows,
            active,
            display: ScreenRect::from_cg(screen.bounds),
            usable: ScreenRect::from_cg(screen.frame),
            tiling: ScreenRect::from_cg(crate::layout::domain::area::compute_tiling_area(
                screen.frame,
                &gaps,
            )),
            scroll,
        })
    }

    fn overview_row(
        &self,
        space: SpaceId,
        workspace: VirtualWorkspaceId,
        screen: &ScreenInfo,
        gaps: &crate::layout::settings::Gaps,
    ) -> RowInput {
        let engine = &self.layout_engine;
        let windows = &self.state.windows;
        let frames: HashMap<WindowId, CGRect> = engine
            .preview_layout_for_workspace(windows, space, workspace, screen.frame, gaps)
            .into_iter()
            .collect();
        let laid = |window: WindowId| {
            Some(Laid {
                window,
                frame: ScreenRect::from_cg(*frames.get(&window)?),
            })
        };
        let columns = engine
            .strip_columns(space, workspace)
            .into_iter()
            .map(|column| ColumnInput {
                windows: column.into_iter().filter_map(laid).collect(),
            })
            .filter(|column| !column.windows.is_empty())
            .collect();
        let mut floating: Vec<Laid> = frames
            .keys()
            .copied()
            .filter(|window| engine.is_window_floating(*window))
            .filter_map(laid)
            .collect();
        floating.sort_by_key(|laid| laid.window);
        let in_workspace = |window: &WindowId| {
            engine.virtual_workspace_manager().workspace_for_window(windows, space, *window)
                == Some(workspace)
        };
        let current = engine
            .focused_window()
            .filter(in_workspace)
            .or_else(|| engine.active_window(windows, space, workspace));
        RowInput { columns, floating, current }
    }

    /// The scene the open overview was last laid out as.
    #[cfg(test)]
    pub(crate) fn overview_scene(&self) -> Option<&crate::overview::domain::scene::Scene> {
        self.overview.live.as_ref().map(|live| &live.view.scene)
    }

    /// The presses the open overview's scene last answered.
    #[cfg(test)]
    pub(crate) fn overview_presses(&self) -> &[Press] {
        self.overview.live.as_ref().map_or(&[], |live| &live.presses)
    }
}
