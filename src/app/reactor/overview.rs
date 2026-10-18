//! The overview's session on the reactor's side: opened and closed by the tap's hold
//! (`input::domain::switch_session`), its scene laid out from the real layout as it opens and after
//! every batch while it is open, every flight a press inside it asks for held back with its frames
//! placed at once, and the mouse kept from moving focus or the pointer until it closes. Nothing
//! draws the scene yet (`specs/overview.md`).

use objc2_core_foundation::{CGPoint, CGRect};
use rini_core::ids::{SpaceId, WindowId};
use rini_geometry::space::{FromCg, OverlayVector, ScreenRect, ToCg};
use rustc_hash::{FxHashMap as HashMap, FxHashSet as HashSet};
use tracing::{debug, info};

use super::events::EventOutcome;
use super::{Reactor, ScreenInfo};
use crate::animation::domain::choreography::Cue;
use crate::input::domain::switch_session::OverviewSignal;
use crate::overview::domain::scene::{ColumnInput, Laid, OverviewInput, RowInput, Scene, lay_out};
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
    /// The space of the display it shows.
    space: SpaceId,
    /// The scene as last laid out.
    scene: Scene,
    /// The press a held flight named since the scene was last laid out.
    pending: Option<Press>,
    /// The press the scene last laid out answers, if a press changed it.
    press: Option<Press>,
    /// Where the pointer goes as it closes: the last warp asked for while it was open.
    warp: Option<Warp>,
}

/// A press inside the overview, as the flight it held back named it: what the scene's next change
/// plays.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Press {
    /// A layout pass, a move or a step along the strip, or a workspace switch.
    Moved(Cue),
    /// A push against an end of the strip or of the stack, giving by `overshoot`.
    Pushed {
        press: Cue,
        overshoot: OverlayVector,
    },
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
            OverviewSignal::Open => self.open_overview(),
            OverviewSignal::Close => self.close_overview(),
        }
        EventOutcome::no_change()
    }

    /// Opens the overview on the focused display, and warms every workspace of it so its thumbs
    /// have pictures. Refused, and the tap's session ended, where that display shows a space rini
    /// does not manage, a native full-screen one included.
    fn open_overview(&mut self) {
        if self.overview.live.is_some() {
            return;
        }
        let scene = self
            .overview_display()
            .and_then(|space| Some((space, self.lay_out_overview(space, 0.0)?)));
        let Some((space, scene)) = scene else {
            info!("overview refused: the focused display shows no space rini manages");
            self.end_tap_session();
            return;
        };
        self.overview.live = Some(LiveOverview {
            space,
            scene,
            pending: None,
            press: None,
            warp: None,
        });
        self.warm_all_workspaces(space);
    }

    /// Closes the overview, carrying out the last warp asked for while it was open. Focus is where
    /// its last press left it, which is what the scene selected.
    fn close_overview(&mut self) {
        let Some(live) = self.overview.live.take() else {
            return;
        };
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
        let (space, scroll) = (live.space, live.scene.scroll);
        let scene = if self.focus_left_overview(space) {
            None
        } else {
            self.lay_out_overview(space, scroll)
        };
        let Some(scene) = scene else {
            info!("overview closed: focus left its display, or the display is gone");
            self.close_overview();
            self.end_tap_session();
            return;
        };
        let Some(live) = self.overview.live.as_mut() else {
            return;
        };
        if scene == live.scene && live.pending.is_none() {
            return;
        }
        live.press = live.pending.take();
        debug!(selected = ?scene.selected, press = ?live.press, "overview laid out again");
        live.scene = scene;
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
            live.pending = Some(press);
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

    /// Tells the tap its session is over from here, so it stops holding keys for it.
    pub(super) fn end_tap_session(&self) {
        if let Some(event_tap_tx) = &self.communication_manager.event_tap_tx {
            event_tap_tx.send(crate::input::platform::input_tap::Request::EndSwitchSession);
        }
    }

    /// The space of the display the user is on: where the key window is, else the layout's focus,
    /// else the space commands act on.
    fn overview_display(&self) -> Option<SpaceId> {
        self.main_window()
            .or_else(|| self.layout_engine.focused_window())
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
    /// `scroll` as it was last laid out; none where it shows no space rini manages.
    fn lay_out_overview(&mut self, space: SpaceId, scroll: f64) -> Option<Scene> {
        if !self.is_space_active(space) {
            return None;
        }
        Some(lay_out(&self.overview_input(space, scroll)?))
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
    pub(crate) fn overview_scene(&self) -> Option<&Scene> {
        self.overview.live.as_ref().map(|live| &live.scene)
    }

    /// The press the open overview's scene last answered.
    #[cfg(test)]
    pub(crate) fn overview_press(&self) -> Option<Press> {
        self.overview.live.as_ref().and_then(|live| live.press)
    }
}
