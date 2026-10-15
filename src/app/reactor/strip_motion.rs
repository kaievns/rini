//! The strip's motion on the overlay: overlay frames placed, workspaces warmed, a pan along the
//! strip, an edge bounce and a workspace switch started, the movements a pass takes, and what the
//! transitions engine is told about the display and its journal.

use objc2_core_foundation::{CGPoint, CGRect};
use rini_core::ids::{SpaceId, WindowId, WindowServerId};
use rini_geometry::space::{
    FromCg, ScreenPoint, ScreenRect, ScreenSize, SurfacePoint, SurfaceRect, ToCg,
};
use rustc_hash::FxHashMap as HashMap;
use tracing::debug;

use super::{Reactor, ScreenInfo};
use crate::transitions::domain::request::SurfaceWindow;
use crate::windows::domain::request::{Request, SuppressEnhancedUi};
use crate::workspaces::Direction;

impl Reactor {
    /// Places windows at their final frames immediately, with no animation.
    ///
    /// Called when the overlay animation is far enough along that the real windows are hidden behind
    /// it. Each write is a synchronous request into another process, and those land at different
    /// times, but that no longer matters: nothing is visible until the overlay comes down.
    pub(super) fn apply_overlay_frames(&mut self, frames: Vec<(WindowId, CGRect)>) {
        let display = self
            .space_state
            .screens
            .iter()
            .find(|screen| screen.space == self.active_display_space())
            .or_else(|| self.space_state.screens.first())
            .map_or(CGRect::ZERO, |screen| screen.frame);
        // A park-to-park move is not sent: nothing visible changes, and the app's repaint would
        // stall the compositor under the flight. Judged from where the window server says the
        // window IS, not from the model: a write the app dropped leaves the model saying "parked"
        // while the window sits on screen, and skipping on the model kept it there. One query for
        // every window rather than one each.
        let server_ids: Vec<WindowServerId> = frames
            .iter()
            .filter_map(|(wid, _)| self.state.windows.window(*wid)?.info.sys_id)
            .collect();
        let real: HashMap<WindowServerId, ScreenRect> = (self.reads.window_server)
            .get_windows(&server_ids)
            .into_iter()
            .map(|info| (info.id, ScreenRect::from_cg(info.frame)))
            .collect();
        let (writes, skipped) = crate::transitions::domain::frame_writes::writes_to_send(
            frames
                .into_iter()
                .map(|(wid, frame)| (wid, ScreenRect::from_cg(frame)))
                .collect(),
            ScreenRect::from_cg(display),
            |wid| {
                let wsid = self.state.windows.window(wid)?.info.sys_id?;
                real.get(&wsid).copied()
            },
        );
        for (wid, frame) in writes {
            let frame = frame.to_cg();
            let Some(window) = self.state.windows.window_mut(wid) else {
                continue;
            };
            let wsid = window.info.sys_id;
            window.frame_monotonic = frame;
            let txid = wsid.map(|wsid| self.transactions.next_txid(wsid)).unwrap_or_default();
            if let Some(wsid) = wsid {
                self.transactions.insert(wsid, txid, frame);
            }
            if let Some(app) = self.apps.get(&wid.pid) {
                _ = app.handle.send(Request::SetWindowFrame {
                    window: wid,
                    frame,
                    txid,
                    enhanced_ui: SuppressEnhancedUi::Yes,
                });
            }
        }
        if skipped > 0 {
            debug!(skipped, "park-to-park frames not sent");
        }
    }

    /// Queues background captures for every window on every workspace of this space.
    ///
    /// A workspace switch is only legible when BOTH strips are drawn. Warming just the windows in the
    /// current animation left the destination blank, so a switch looked like a one-way slide into
    /// nothing and gave no sense of direction at all.
    ///
    /// Cheap to call repeatedly: the service drops targets already in flight and the cache keeps what
    /// it holds unless something better arrives, so this settles rather than re-capturing. During a
    /// flight the pictures service holds the targets until it settles ("Capture work in flight" in
    /// `src/transitions/docs/coordination.md`), so calling this from the switch handler is safe.
    fn warm_all_workspaces(&mut self, space: SpaceId) {
        let Some(pictures) = self.communication_manager.pictures.clone() else {
            return;
        };
        let Some(screen) = self
            .space_state
            .screens
            .iter()
            .find(|s| s.space == Some(space))
            .or_else(|| self.space_state.screens.first())
            .cloned()
        else {
            return;
        };
        let workspaces = self.layout_engine.virtual_workspace_manager_mut().list_workspaces(space);
        let gaps = self
            .config
            .settings
            .layout
            .gaps
            .effective_for_display(screen.display_uuid_opt());

        let mut targets: Vec<crate::pictures::domain::target::PictureTarget> = Vec::new();
        for (workspace_id, _) in &workspaces {
            let layout = self.layout_engine.calculate_layout_for_workspace(
                &self.state.windows,
                space,
                *workspace_id,
                screen.frame,
                &gaps,
            );
            for (wid, frame) in layout {
                let Some(window) = self.state.windows.window(wid) else {
                    continue;
                };
                let Some(server_id) = window.info.sys_id else { continue };
                targets.push(crate::pictures::domain::target::PictureTarget {
                    window: wid,
                    server_id,
                    size: ScreenSize::from_cg(frame.size),
                });
            }
        }
        if targets.is_empty() {
            return;
        }
        pictures.ask(crate::pictures::domain::schedule::Ask {
            priority: crate::pictures::domain::schedule::Priority::Warm,
            targets,
        });
    }

    /// The workspace indices this pass moves the space between, from the one the last pass drew to the
    /// active one, which is drawn from now on. `None` when it stays on one workspace or has none drawn.
    ///
    /// Every pass takes it, not only a switch's: a space has to have an origin before its first switch.
    pub(super) fn take_workspace_movement(&mut self, space: SpaceId) -> Option<(usize, usize)> {
        let active = self.layout_engine.active_workspace(space)?;
        let drawn = self.drawn_workspace.insert(space, active)?;
        if drawn == active {
            return None;
        }
        let workspaces = self.layout_engine.virtual_workspace_manager_mut().list_workspaces(space);
        let index = |workspace| workspaces.iter().position(|(id, _)| *id == workspace);
        Some((index(drawn)?, index(active)?))
    }

    /// Animates a strip scroll as one horizontal viewport pan, the horizontal twin of a workspace
    /// switch, and a move along the strip as that pan with a swap. Returns true when the strip
    /// surface took over.
    ///
    /// `delta` is how far the strip moved, zero when it did not. The strip surface holds the whole
    /// active workspace at its final positions, and the viewport starts shifted back by `delta` so
    /// the strip appears to arrive from where it was, then settles. A `moved` window is the press's
    /// cue, and the look stages it (`strip_move::stage_move`): Slide starts the moved column and the
    /// column it passed from the slots they held before the move, so the two are seen changing
    /// places, unless the moved column fills the viewport, when the whole strip steps and the column
    /// it passed is held on the strip beneath it. See "The move flight, staged" in
    /// `src/transitions/docs/coordination.md`.
    ///
    /// `final_frames` are the pass's windows on this workspace whose frames changed: the real windows
    /// placed once the surface covers them. A window that did not move is not re-placed, as on the
    /// per-window path. `standing` is where the pass found each window and `laid_out` where the
    /// strip had it at the last pass: one away from where the shifted viewport draws it starts where
    /// it stands if on screen, else where the strip had it (`strip_move::start_where_standing`).
    #[allow(clippy::too_many_arguments, reason = "no roadmap task groups them")]
    pub(super) fn start_strip_pan(
        &mut self,
        space: SpaceId,
        workspace_id: crate::workspaces::VirtualWorkspaceId,
        final_frames: Vec<(WindowId, CGRect)>,
        delta: CGPoint,
        moved: Option<(WindowId, Direction)>,
        standing: &[crate::transitions::domain::request::AnimationRequest],
        laid_out: &[(WindowId, CGRect)],
    ) -> bool {
        if self.communication_manager.transitions_tx.is_none() {
            return false;
        }
        let Some((screen, gaps)) = self.strip_screen(space) else {
            return false;
        };
        let full = self.layout_engine.calculate_layout_for_workspace(
            &self.state.windows,
            space,
            workspace_id,
            screen.frame,
            &gaps,
        );

        // Full-display coordinates, matching the overlay's own space.
        let display_bounds = (self.reads.display_bounds)(screen.id);
        // A floating window is not in the strip. Panning it with the strip dragged it sideways
        // and snapped it back at the handover.
        let mut windows = self.strip_windows_at(&full, display_bounds, true);
        if windows.is_empty() {
            return false;
        }

        // The strip arrives from where it was: start the viewport shifted by the movement, settle at
        // zero.
        //
        // The sign matters and was wrong. A tile at strip-surface coordinate c appears on screen at
        // c - offset, and the strip surface is built from the NEW layout, so at t = 0 the tile must appear
        // where the window currently IS:
        //
        //     new_x - offset = old_x,  and  new_x = old_x + delta,  so  offset = delta
        //
        // Using -delta started the pan on the wrong side and moved it the wrong way. Chaining then
        // compounded the error on every press, which is what made rapid next/prev jerk back and forth.
        let from_offset = SurfacePoint::new(delta.x, delta.y);
        let to_offset = SurfacePoint::origin();
        use crate::animation::domain::choreography::{Cue, Look};
        let look = Look::default();
        let cue = moved.map_or(Cue::Plain, |(window, direction)| Cue::Move { window, direction });
        // A move's pair starts where the look stages it.
        if cue == Cue::Plain {
            start_where_standing(
                &mut windows,
                standing,
                laid_out,
                from_offset,
                display_bounds,
                screen.frame,
            );
        }
        let area = strip_area(screen.frame, display_bounds, &gaps);
        // The plain duration: the look stretches the flight only when it takes the step.
        let duration =
            std::time::Duration::from_secs_f64(self.config.settings.animation_duration.max(0.0));
        let accent = crate::transitions::domain::strip_move::stage_move(
            &mut windows,
            &cue,
            area,
            crate::animation::domain::motion::surface::pan_travel(from_offset, to_offset),
            duration.as_secs_f64(),
            look,
        );

        self.send_flight(
            space,
            crate::transitions::platform::engine::Event::AnimateSurface {
                windows,
                from_offset,
                to_offset,
                final_frames: final_frames
                    .into_iter()
                    .map(|(wid, frame)| (wid, ScreenRect::from_cg(frame)))
                    .collect(),
                focus: self.layout_engine.focused_window(),
                focused_before: self.focused_as_event_began,
                key: self.main_window_tracker.key_window(),
                duration,
                look,
                cue,
                accent,
                cause: self.cause,
            },
        );
        true
    }

    /// The strip surface for one workspace's layout: every window with a server id, at its layout
    /// frame on the surface's first row. `pin_floating` holds floating windows still while the strip
    /// moves under them (a pan); a vertical movement carries them with their workspace.
    fn strip_windows_at(
        &self,
        layout: &[(WindowId, CGRect)],
        display_bounds: CGRect,
        pin_floating: bool,
    ) -> Vec<crate::transitions::domain::request::SurfaceWindow> {
        let mut windows = Vec::with_capacity(layout.len());
        let rings = self.layout_rings();
        for (wid, frame) in layout {
            let Some(window) = self.state.windows.window(*wid) else {
                continue;
            };
            let Some(server_id) = window.info.sys_id else { continue };
            let floating = self.layout_engine.is_window_floating(*wid);
            windows.push(crate::transitions::domain::request::SurfaceWindow {
                window: *wid,
                server_id,
                frame: crate::transitions::domain::strip_stack::strip_frame(
                    ScreenRect::from_cg(*frame),
                    ScreenPoint::from_cg(display_bounds.origin),
                    0,
                    0.0,
                ),
                pinned: pin_floating && floating,
                floating,
                from: None,
                worn: self.worn(*wid, &rings),
            });
        }
        windows
    }

    /// Bounces the view against the end a command ran into: the strip's first or last column
    /// (`Left`/`Right`), or the top or bottom of the workspace stack (`Up`/`Down`). The active
    /// workspace's surface nudges `EDGE_BOUNCE_OVERSHOOT` the way the view was pushed and returns;
    /// the real windows do not move. See "Edge bounce" in `src/transitions/docs/coordination.md`.
    pub(super) fn start_edge_bounce(&mut self, space: SpaceId, direction: Direction) {
        if !self.config.settings.animate || (self.reads.low_power)() {
            return;
        }
        if self.communication_manager.transitions_tx.is_none() {
            return;
        }
        let Some(screen) = self
            .space_state
            .screens
            .iter()
            .find(|s| s.space == Some(space))
            .or_else(|| self.space_state.screens.first())
            .cloned()
        else {
            return;
        };
        let Some(workspace_id) = self.layout_engine.active_workspace(space) else {
            return;
        };
        let gaps = self
            .config
            .settings
            .layout
            .gaps
            .effective_for_display(screen.display_uuid_opt());
        let layout = self.layout_engine.calculate_layout_for_workspace(
            &self.state.windows,
            space,
            workspace_id,
            screen.frame,
            &gaps,
        );
        let display_bounds = (self.reads.display_bounds)(screen.id);
        let vertical = matches!(direction, Direction::Up | Direction::Down);
        // A strip bounce leaves floating windows where they are, as a pan does; a stack bounce
        // carries them, as a switch does.
        let windows = self.strip_windows_at(&layout, display_bounds, !vertical);
        if windows.is_empty() {
            return;
        }
        let overshoot = crate::animation::domain::motion::plan::edge_bounce_overshoot(direction);
        let duration =
            std::time::Duration::from_secs_f64(self.config.settings.animation_duration.max(0.0));
        tracing::debug!(?direction, windows = windows.len(), "edge bounce");
        self.send_flight(space, crate::transitions::platform::engine::Event::Bounce {
            windows,
            overshoot,
            final_frames: layout
                .into_iter()
                .map(|(wid, frame)| (wid, ScreenRect::from_cg(frame)))
                .collect(),
            focus: self.layout_engine.focused_window(),
            duration,
            look: crate::animation::domain::choreography::Look::default(),
            cue: crate::animation::domain::choreography::Cue::Plain,
            cause: self.cause,
        });
    }

    /// Builds and starts a strip-surface animation for a workspace switch, if one is warranted.
    ///
    /// Returns true when the strip surface took over, in which case the caller must not touch the real
    /// windows: the flight engine asks for them once it is covering them.
    ///
    /// Every workspace between the two is laid out and stacked below the one above it, so a jump from
    /// 1 to 4 scrolls past 2 and 3. Without that, a four-workspace jump looks exactly like a
    /// one-workspace step, which gives no sense of where you have moved to.
    pub(super) fn start_strip_switch(
        &mut self,
        space: SpaceId,
        from_index: usize,
        to_index: usize,
        layout: &[(WindowId, CGRect)],
        skip_wid: Option<WindowId>,
    ) -> bool {
        if from_index == to_index {
            return false;
        }
        tracing::debug!(from_index, to_index, "strip switch requested");
        if self.communication_manager.transitions_tx.is_none() {
            return false;
        }
        let Some((screen, gaps)) = self.strip_screen(space) else {
            return false;
        };

        let workspaces = self.layout_engine.virtual_workspace_manager_mut().list_workspaces(space);
        if workspaces.is_empty() {
            return false;
        }

        let display_bounds = (self.reads.display_bounds)(screen.id);
        let height = screen.frame.size.height;
        let (windows, destination_layout) = self.switch_rows(
            space,
            &workspaces,
            (from_index, to_index),
            &screen,
            display_bounds,
            &gaps,
        );

        // EVERY window in the layout, not just the destination workspace's. This layout is what the
        // non-animated path would have applied, and it includes the parked positions of windows
        // belonging to other workspaces. Placing only the destination's windows left all the others
        // wherever they happened to be, so windows from one workspace showed up as phantoms on
        // another until something else moved the strip.
        let final_frames: Vec<(WindowId, ScreenRect)> = layout
            .iter()
            .filter(|(wid, _)| Some(*wid) != skip_wid)
            .map(|&(wid, frame)| (wid, ScreenRect::from_cg(frame)))
            .collect();

        tracing::debug!(
            from_index,
            to_index,
            workspaces = workspaces.len(),
            strip_windows = windows.len(),
            final_frames = final_frames.len(),
            height,
            "strip switch build"
        );
        if windows.is_empty() {
            return false;
        }

        let travel = crate::transitions::domain::strip_stack::travel(from_index, to_index, height);
        let (from_offset, to_offset) = (travel.from, travel.to);
        let duration = std::time::Duration::from_secs_f64(
            (self.config.settings.animation_duration.max(0.0)) * travel.duration_stretch,
        );

        self.claim_destination_strip(space, destination_layout);
        report_target_out_of_view(self.layout_engine.focused_window(), &windows, display_bounds);

        self.send_flight(
            space,
            crate::transitions::platform::engine::Event::AnimateSurface {
                windows,
                from_offset,
                to_offset,
                final_frames,
                focus: self.layout_engine.focused_window(),
                focused_before: self.focused_as_event_began,
                key: self.main_window_tracker.key_window(),
                duration,
                look: crate::animation::domain::choreography::Look::default(),
                cue: crate::animation::domain::choreography::Cue::Plain,
                accent: None,
                cause: self.cause,
            },
        );
        // Every workspace, so the next switch in any direction has both strips drawn. Stays here:
        // the pictures service holds a warm until the quiet after the flight (`src/transitions/docs/coordination.md`).
        self.warm_all_workspaces(space);
        true
    }

    /// Every workspace from `from_index` to `to_index`, laid out and stacked one usable height
    /// below the one above it, as strip-surface windows; and the destination's own layout.
    fn switch_rows(
        &mut self,
        space: SpaceId,
        workspaces: &[(crate::workspaces::VirtualWorkspaceId, String)],
        (from_index, to_index): (usize, usize),
        screen: &ScreenInfo,
        display_bounds: CGRect,
        gaps: &crate::layout::settings::Gaps,
    ) -> (Vec<SurfaceWindow>, Vec<(WindowId, CGRect)>) {
        let (low, high) = (from_index.min(to_index), from_index.max(to_index));
        // Strips are stacked one USABLE height apart, so each workspace's tiling area sits directly
        // on the one above it and a switch reads as the strips scrolling as a single column. The
        // pitch was once the full display height, which left a menu-bar-sized band of empty desktop
        // between one workspace and the next as it scrolled past.
        //
        // The surface is still expressed in full-display coordinates (the overlay's own), so a
        // window keeps its position below the bar within its row; only the row-to-row step shrinks.
        let row_pitch = screen.frame.size.height;
        let mut windows: Vec<SurfaceWindow> = Vec::new();
        let mut destination_layout = Vec::new();
        let rings = self.layout_rings();
        for index in low..=high {
            let Some((workspace_id, _)) = workspaces.get(index) else {
                continue;
            };
            let layout = self.layout_engine.calculate_layout_for_workspace(
                &self.state.windows,
                space,
                *workspace_id,
                screen.frame,
                gaps,
            );
            if index == to_index {
                destination_layout.clone_from(&layout);
            }
            // Stacked below the workspace above it, separated by the menu bar inset, and expressed
            // relative to the display's own origin so the overlay's space needs no further translation.
            let row = crate::transitions::domain::strip_stack::row_of(index, from_index, to_index);
            for (wid, frame) in layout {
                let Some(window) = self.state.windows.window(wid) else {
                    continue;
                };
                let Some(server_id) = window.info.sys_id else { continue };
                windows.push(SurfaceWindow {
                    window: wid,
                    server_id,
                    frame: crate::transitions::domain::strip_stack::strip_frame(
                        ScreenRect::from_cg(frame),
                        ScreenPoint::from_cg(display_bounds.origin),
                        row,
                        row_pitch,
                    ),
                    // Never pinned here. A floating window belongs to a workspace, so it leaves with the
                    // one being left and arrives with the one being entered.
                    pinned: false,
                    floating: self.layout_engine.is_window_floating(wid),
                    from: None,
                    worn: self.worn(wid, &rings),
                });
            }
        }

        (windows, destination_layout)
    }

    /// The destination's strip claimed by the switch.
    ///
    /// The switch owns the destination's horizontal position. Its row is drawn at that scroll already, so
    /// the strip arrives with the target window in view and the only movement the eye sees is vertical.
    /// Without claiming it, the next pass reads the destination's origin as a fresh movement and pans
    /// sideways after the slide has landed. Its layout with it, as the strip the next pan starts from.
    fn claim_destination_strip(
        &mut self,
        space: SpaceId,
        destination_layout: Vec<(WindowId, CGRect)>,
    ) {
        if let Some(destination) = self.layout_engine.active_workspace(space)
            && let Some(origin) = self.layout_engine.strip_origin(space)
        {
            self.last_strip_origin.insert((space, destination), origin);
            self.last_strip_layout.insert((space, destination), destination_layout);
        }
    }

    /// How far the ACTIVE workspace's strip has moved on screen since a pass last looked at that strip,
    /// and remember where it is now: the change of its origin, the anchor less the scroll offset, so a
    /// change of anchor with the offset unchanged is a movement too.
    ///
    /// `None` when the space has no strip. `Some(0)` is meaningful and different: it says the strip did not
    /// move, so whatever else the layout did is not a pan. Windows move WITH the origin: scrolling the
    /// viewport further along the strip lowers it and carries the windows left.
    ///
    /// The first look at a workspace reports no movement, which is what a switch onto it needs: the switch
    /// itself is vertical, and the destination's own scroll is drawn into the strip surface rather than panned to.
    pub(crate) fn take_strip_movement(&mut self, space: SpaceId) -> Option<CGPoint> {
        let workspace = self.layout_engine.active_workspace(space)?;
        let now = self.layout_engine.strip_origin(space)?;
        let before = self.last_strip_origin.insert((space, workspace), now);
        tracing::debug!(
            now,
            before = before.unwrap_or(now),
            focused = format!("{:?}", self.layout_engine.focused_window()),
            "strip movement"
        );
        Some(CGPoint::new(now - before.unwrap_or(now), 0.0))
    }

    /// Where the ACTIVE workspace's strip had each window the last time a pass looked, `None` on the
    /// first look at a workspace, and where it has them now, which it remembers: every window at its
    /// strip frame, a parked one where the strip has it rather than at its park. Neither when
    /// `space` has no active workspace or screen.
    #[allow(clippy::type_complexity, reason = "P4.1")]
    pub(crate) fn take_strip_layout(
        &mut self,
        space: SpaceId,
    ) -> Option<(Option<Vec<(WindowId, CGRect)>>, Vec<(WindowId, CGRect)>)> {
        let workspace = self.layout_engine.active_workspace(space)?;
        let (screen, gaps) = self.strip_screen(space)?;
        let now = self.layout_engine.calculate_layout_for_workspace(
            &self.state.windows,
            space,
            workspace,
            screen.frame,
            &gaps,
        );
        Some((
            self.last_strip_layout.insert((space, workspace), now.clone()),
            now,
        ))
    }

    /// The screen `space` is laid out on, the first one when it is on none, and its gaps.
    fn strip_screen(&self, space: SpaceId) -> Option<(ScreenInfo, crate::layout::settings::Gaps)> {
        let screen = self
            .space_state
            .screens
            .iter()
            .find(|s| s.space == Some(space))
            .or_else(|| self.space_state.screens.first())
            .cloned()?;
        let gaps = self
            .config
            .settings
            .layout
            .gaps
            .effective_for_display(screen.display_uuid_opt());
        Some((screen, gaps))
    }

    /// The window the current command moved along the strip, if this pass lays it out; taken, so
    /// only one pass draws the move.
    pub(crate) fn take_move_in(
        &mut self,
        layout: &[(WindowId, CGRect)],
    ) -> Option<(WindowId, Direction)> {
        let (window, _) = self.moved_along_strip?;
        if !layout.iter().any(|(wid, _)| *wid == window) {
            return None;
        }
        self.moved_along_strip.take()
    }

    pub(crate) fn publish_animation_display(&self) { self.publish_animation_display_for(None); }

    /// Hands the flight engine `event`, a flight over the display holding `space`, told first which
    /// display that is, and notes the display and the press for the rest windows, which hold what the
    /// flight changes from now (`publish_rest`). Whether the engine runs to take it.
    pub(super) fn send_flight(
        &mut self,
        space: SpaceId,
        event: crate::transitions::platform::engine::Event,
    ) -> bool {
        let display = self.publish_animation_display_for(Some(space));
        let Some(tx) = &self.communication_manager.transitions_tx else {
            return false;
        };
        let pressed = event.pressed();
        tx.send(event);
        self.rest.flight = display.zip(pressed).or(self.rest.flight);
        true
    }

    /// The transitions engine the reactor drives, told at once whether to write its journal and
    /// measure each lift's handover: it writes nothing until it hears.
    pub(super) fn connect_transitions(
        &mut self,
        transitions_tx: Option<crate::transitions::platform::engine::Sender>,
    ) {
        self.communication_manager.transitions_tx = transitions_tx;
        self.publish_journal();
    }

    /// Tells the transitions engine whether to write its journal and measure each lift's handover.
    pub(super) fn publish_journal(&self) {
        if let Some(tx) = &self.communication_manager.transitions_tx {
            tx.send(crate::transitions::platform::engine::Event::Journal(
                self.config.settings.journal,
            ));
        }
    }

    /// Points the animation overlay at the display holding `space`.
    ///
    /// There is one overlay, so it has to sit on the display whose windows are about to move. Choosing the
    /// ACTIVE display instead put a built-in workspace switch on the external screen whenever the cursor
    /// was over there: the external showed the built-in's windows sliding while the built-in's own windows
    /// snapped with no animation. `space` is `None` only for a config reload, which has no particular
    /// display in mind. The display's bounds, as the overlay covers them.
    pub(crate) fn publish_animation_display_for(
        &self,
        space: Option<SpaceId>,
    ) -> Option<ScreenRect> {
        let wanted = space.or_else(|| self.active_display_space());
        let screen = self
            .space_state
            .screens
            .iter()
            .find(|screen| screen.space == wanted)
            .or_else(|| self.space_state.screens.first())?;
        let frame = ScreenRect::from_cg((self.reads.display_bounds)(screen.id));
        if let Some(tx) = &self.communication_manager.transitions_tx {
            tx.send(crate::transitions::platform::engine::Event::SetDisplay {
                id: screen.id.as_u32(),
                frame,
                scale: screen.scale,
                top_band: self.config.settings.top_band(),
                rests: self.config.settings.decorations.draws_bands(),
            });
        }
        if let Some(pictures) = &self.communication_manager.pictures {
            pictures.set_display(
                crate::pictures::domain::schedule::Display {
                    id: screen.id.as_u32(),
                    frame,
                    scale: screen.scale,
                },
                !self.config.settings.bar.enabled,
            );
        }
        Some(frame)
    }
}

/// Says so when the window a switch focuses is off the destination row's viewport.
///
/// The switch's claim on the destination strip suppresses the pan that would otherwise reveal the
/// target, so if the destination row was NOT drawn scrolled to it, the target stays off screen.
/// That would be a layout problem rather than an animation one, and it is silent, so say so.
fn report_target_out_of_view(
    focused: Option<WindowId>,
    windows: &[SurfaceWindow],
    display_bounds: CGRect,
) {
    if let Some(target) = focused
        && let Some(placed) = windows.iter().find(|window| window.window == target)
        && !crate::transitions::domain::strip_stack::lands_in_view(
            placed.frame,
            display_bounds.size.width,
        )
    {
        tracing::debug!(
            idx = target.idx.get(),
            x = placed.frame.origin.x,
            width = display_bounds.size.width,
            "the switch target is off the destination row's viewport and will not be revealed"
        );
    }
}

/// Each window of a plain pan started where it stands if on screen, else where the strip had it,
/// both on the surface at rest (`strip_move::start_where_standing`).
fn start_where_standing(
    windows: &mut [SurfaceWindow],
    standing: &[crate::transitions::domain::request::AnimationRequest],
    laid_out: &[(WindowId, CGRect)],
    from_offset: SurfacePoint,
    display_bounds: CGRect,
    screen_frame: CGRect,
) {
    let origin = ScreenPoint::from_cg(display_bounds.origin);
    let at_rest =
        |frame| crate::transitions::domain::strip_stack::strip_frame(frame, origin, 0, 0.0);
    let standing: Vec<(WindowId, SurfaceRect)> =
        standing.iter().map(|request| (request.window, at_rest(request.from))).collect();
    let laid_out: Vec<(WindowId, SurfaceRect)> = laid_out
        .iter()
        .map(|&(window, frame)| (window, at_rest(ScreenRect::from_cg(frame))))
        .collect();
    crate::transitions::domain::strip_move::start_where_standing(
        windows,
        &standing,
        &laid_out,
        from_offset,
        at_rest(ScreenRect::from_cg(screen_frame)),
    );
}

/// The area a look stages a strip move in: the screen's frame in the overlay's space, with the
/// strip's outer gaps.
fn strip_area(
    screen_frame: CGRect,
    display_bounds: CGRect,
    gaps: &crate::layout::settings::Gaps,
) -> crate::animation::domain::choreography::Area {
    crate::animation::domain::choreography::Area {
        frame: rini_geometry::to_overlay_space(
            ScreenRect::from_cg(screen_frame),
            ScreenRect::from_cg(display_bounds),
        ),
        outer_left: gaps.outer.left,
        outer_right: gaps.outer.right,
    }
}
