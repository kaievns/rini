//! The reactor's half of the decorations: what each managed window wears, who wears the ring, and
//! what the rest windows are told.
//!
//! The ring is decided over rini's own stores (`decorations::domain::style::Rings`): each
//! workspace's active window, the one focus lands on there, on every display. The rings the last
//! batch of events left are kept, so a pass can say whether each window wore the ring before it.
//! At rest the window holding focus is the key window as macOS reports it, so a band turns when
//! macOS repaints its window's focus, not at rini's decision; after every batch the rest windows are
//! sent what each managed window wears and what holds its band, when that changed.

use rini_core::ids::{SpaceId, WindowId};
use rini_geometry::is_off_screen;
use rini_geometry::space::{FromCg, ScreenRect};

use crate::decorations::domain::rest::{Held, RestDisplay, RestModel, RestWindow};
use crate::decorations::domain::style::{Decoration, Rings, Worn};
use crate::decorations::platform::rest::{Event as RestEvent, Sender};
use crate::workspaces::VirtualWorkspaceId;

use super::Reactor;

/// Where the rest windows are drawn, and the model they were last sent.
#[derive(Default)]
pub(super) struct RestFeed {
    /// `None` until the main thread's rest windows are wired in, and in tests that do not look.
    pub(super) tx: Option<Sender>,
    sent: Option<RestModel>,
}

impl Reactor {
    /// The rings with `focus` holding focus: each workspace's active window on every display rini
    /// manages, `focus` on its own workspace, which a border's paint reads too. None while rini
    /// draws neither, so nothing is read.
    pub(super) fn rings_with(&self, focus: Option<WindowId>) -> Rings {
        if self.config.settings.decorations.decoration() == Decoration::default() {
            return Rings::default();
        }
        let engine = &self.layout_manager.layout_engine;
        let workspaces = engine.virtual_workspace_manager();
        let active: Vec<((SpaceId, VirtualWorkspaceId), Option<WindowId>)> = self
            .space_state
            .screens
            .iter()
            .filter_map(|screen| screen.space)
            .filter(|space| self.is_space_active(*space))
            .flat_map(|space| {
                workspaces.existing_workspaces(space).into_iter().map(move |(workspace, _)| {
                    ((space, workspace), engine.active_window(&self.state.windows, space, workspace))
                })
            })
            .collect();
        let focus = focus.and_then(|window| {
            let info = workspaces.workspace_info_for_window_any(&self.state.windows, window)?;
            Some(((info.space, info.workspace_id), window))
        });
        Rings::of(&active, focus)
    }

    /// The rings as rini's layout has them now: its focused window holding focus.
    pub(super) fn layout_rings(&self) -> Rings {
        self.rings_with(self.layout_manager.layout_engine.focused_window())
    }

    /// What `window` wears through a pass ending with `rings`, against the rings the last batch left.
    pub(super) fn worn(&self, window: WindowId, rings: &Rings) -> Worn {
        Worn {
            decoration: self.config.settings.decorations.decoration(),
            wore_ring: self.rings_drawn.ringed(window),
            wears_ring: rings.ringed(window),
        }
    }
}

impl Reactor {
    /// Sends the rest windows their model if it changed since the last one sent. Once per batch.
    pub(super) fn publish_rest(&mut self) {
        let Some(tx) = &self.rest.tx else { return };
        let model = self.rest_model();
        if self.rest.sent.as_ref() == Some(&model) {
            return;
        }
        tx.send(RestEvent::Model(model.clone()));
        self.rest.sent = Some(model);
    }

    /// Every managed window on a space rini manages, what it wears with the key window holding
    /// focus, and what holds its bands: none of them while rini draws neither ring nor border.
    pub(super) fn rest_model(&self) -> RestModel {
        let screens = &self.space_state.screens;
        let displays = screens
            .iter()
            .map(|screen| RestDisplay { bounds: ScreenRect::from_cg(screen.bounds), scale: screen.scale })
            .collect();
        let decoration = self.config.settings.decorations.decoration();
        if decoration == Decoration::default() {
            return RestModel { displays, windows: Vec::new() };
        }
        let managed = |window: &WindowId| {
            self.state.windows.window(*window).is_some_and(|state| state.is_effectively_manageable())
        };
        let key = self.main_window_tracker.key_window().filter(managed);
        let rings = self.rings_with(key);
        let engine = &self.layout_manager.layout_engine;
        let mission_control = self.is_mission_control_active();
        let affinity = self.affinity();
        let windows = self
            .state
            .windows
            .iter_workspace_assignments()
            .filter(|(window, info)| managed(window) && self.is_space_active(info.space))
            .filter_map(|(window, info)| {
                let server_id = self.state.windows.window(window)?.info.sys_id?;
                let screen = screens.iter().find(|screen| screen.space == Some(info.space));
                let shown_workspace = engine.active_workspace(info.space) == Some(info.workspace_id);
                let off_strip = !engine.is_window_floating(window)
                    && screen.zip(self.last_strip_layout.get(&(info.space, info.workspace_id))).is_some_and(
                        |(screen, strip)| {
                            strip.iter().find(|(w, _)| *w == window).is_some_and(|(_, frame)| {
                                is_off_screen(ScreenRect::from_cg(screen.bounds), ScreenRect::from_cg(*frame))
                            })
                        },
                    );
                Some(RestWindow {
                    window: server_id,
                    decoration,
                    focused: rings.focused(window),
                    ringed: rings.ringed(window),
                    held: Held {
                        parked: !shown_workspace || off_strip,
                        mission_control,
                        fullscreen: affinity.is_known_fullscreen_window(server_id),
                        space_hidden: screen.is_none(),
                    },
                })
            })
            .collect();
        RestModel { displays, windows }
    }
}
