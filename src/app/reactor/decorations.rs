//! The reactor's half of the decorations: what each managed window wears, as the window rules have
//! it, who wears the ring, the border each tiled window gives out of its slot, and what the rest
//! windows are told.
//!
//! The ring is decided over rini's own stores (`decorations::domain::style::Rings`): each
//! workspace's active window, the one focus lands on there, on every display. The rings the last
//! batch of events left are kept, so a pass can say whether each window wore the ring before it.
//! At rest the window holding focus is the key window as macOS reports it, so a band turns when
//! macOS repaints its window's focus, not at rini's decision; after every batch the rest windows are
//! told the display of a flight it handed the engine, so they hold that batch's model and what
//! follows until it lands, then sent what each managed window wears, the workspace it is on and what
//! holds its band, when that changed, and while no window can wear a band they are turned off, told
//! of no flight, and the window server's frames and orders go unfollowed
//! (`DecorationSettings::draws_bands`). Each window is folded afresh whenever what it wears is asked for, so a rule follows focus, floating and the
//! title as they change. A window is drawn urgent while its application asks for attention and no
//! window of it holds focus, read against the same focus as the ring.

use std::time::Instant;

use rini_core::ids::{SpaceId, WindowId};
use rini_geometry::is_off_screen;
use rini_geometry::space::{FromCg, ScreenRect};
use slotmap::Key;

use crate::decorations::domain::rest::{Group, Held, RestDisplay, RestModel, RestWindow};
use crate::decorations::domain::rules;
use crate::decorations::domain::style::{Decoration, Rings, Worn};
use crate::decorations::platform::rest::{Event as RestEvent, Sender};
use crate::displays::event::Following;
use crate::layout::domain::tile::Borders;
use crate::windows::domain::rules::{RuleState, WindowRuleContext};
use crate::workspaces::VirtualWorkspaceId;

use super::Reactor;

/// Where the rest windows are drawn, whether the window server's frames and orders are followed for
/// them, the display of the last flight this batch handed the engine with the press it answers, and
/// the model they were last sent: `None` while they are off, as they start.
#[derive(Default)]
pub(super) struct RestFeed {
    /// `None` until the main thread's rest windows are wired in, and in tests that do not look.
    pub(super) tx: Option<Sender>,
    pub(super) following: Following,
    pub(super) flight: Option<(ScreenRect, Instant)>,
    sent: Option<RestModel>,
}

impl Reactor {
    /// What `window`'s rules match against: its application and its own attributes, and whether it
    /// floats. Whether it holds focus is the fold's to try both ways (`rules::decoration`), and whether
    /// it is urgent `decoration_of`'s to say.
    fn rule_context(&self, window: WindowId) -> WindowRuleContext<'_> {
        let app = self.app_manager.apps.get(&window.pid).map(|app| &app.info);
        let info = self.state.windows.window(window).map(|state| &state.info);
        WindowRuleContext {
            app_bundle_id: app.and_then(|app| app.bundle_id.as_deref()),
            app_name: app.and_then(|app| app.localized_name.as_deref()),
            window_title: info.map(|info| info.title.as_str()),
            ax_role: info.and_then(|info| info.ax_role.as_deref()),
            ax_subrole: info.and_then(|info| info.ax_subrole.as_deref()),
            is_modal: info.is_some_and(|info| info.is_modal),
            state: RuleState {
                floating: self.layout_manager.layout_engine.is_window_floating(window),
                ..RuleState::default()
            },
        }
    }

    /// What `window` wears now, with `rings` naming the window holding focus, so whether it is
    /// drawn urgent.
    pub(super) fn decoration_of(&self, window: WindowId, rings: &Rings) -> Decoration {
        let mut context = self.rule_context(window);
        context.state.urgent = rings.urgent(window, &self.attention);
        rules::decoration(&self.config.settings.decorations, context)
    }

    /// The border each tiled window gives out of its slot, as its display draws it: `layout ›
    /// border`'s at the first display's scale, but for the windows a rule or another display's scale
    /// gives another. None before any display is known, when nothing is laid out.
    pub(super) fn borders(&self) -> Borders {
        let settings = &self.config.settings.decorations;
        let screens = &self.space_state.screens;
        let Some(first) = screens.first().map(|screen| screen.scale) else { return Borders::default() };
        let layout = |scale| rules::border_width(settings, WindowRuleContext::default(), scale);
        let mut borders = Borders::every(layout(first));
        let one_width = screens.iter().all(|screen| layout(screen.scale) == borders.every);
        if one_width && !rules::shape_borders(settings) {
            return borders;
        }
        let workspaces = self.layout_manager.layout_engine.virtual_workspace_manager();
        let scale_of = |window| {
            let space = workspaces.workspace_info_for_window_any(&self.state.windows, window)?.space;
            screens.iter().find(|screen| screen.space == Some(space)).map(|screen| screen.scale)
        };
        borders.windows = self
            .state
            .windows
            .iter_windows()
            .filter(|(_, state)| state.is_effectively_manageable())
            .map(|(window, _)| {
                let scale = scale_of(window).unwrap_or(first);
                (window, rules::border_width(settings, self.rule_context(window), scale))
            })
            .filter(|(_, width)| *width != borders.every)
            .collect();
        borders
    }

    /// Hands the layout the border each window wears now; whether any changed.
    pub(super) fn refresh_borders(&mut self) -> bool {
        let borders = self.borders();
        self.layout_manager.layout_engine.set_borders(borders)
    }

    /// The rings with `focus` holding focus: each workspace's active window on every display rini
    /// manages, `focus` on its own workspace, active on the display `focus` is on and inactive on the
    /// others, each display named by the Space it shows; `focus` is what a border's paint reads. None
    /// while no window can wear a band, so nothing is read.
    pub(super) fn rings_with(&self, focus: Option<WindowId>) -> Rings {
        if !self.config.settings.decorations.draws_bands() {
            return Rings::default();
        }
        let engine = &self.layout_manager.layout_engine;
        let workspaces = engine.virtual_workspace_manager();
        let active: Vec<(SpaceId, VirtualWorkspaceId, Option<WindowId>)> = self
            .space_state
            .screens
            .iter()
            .filter_map(|screen| screen.space)
            .filter(|space| self.is_space_active(*space))
            .flat_map(|space| {
                workspaces.existing_workspaces().into_iter().map(move |(workspace, _)| {
                    (space, workspace, engine.active_window(&self.state.windows, space, workspace))
                })
            })
            .collect();
        let focus = focus.and_then(|window| {
            let info = workspaces.workspace_info_for_window_any(&self.state.windows, window)?;
            Some((info.space, info.workspace_id, window))
        });
        Rings::of(&active, focus)
    }

    /// The rings as rini's layout has them now: its focused window holding focus.
    pub(super) fn layout_rings(&self) -> Rings {
        self.rings_with(self.layout_manager.layout_engine.focused_window())
    }

    /// What `window` wears through a pass ending with `rings`.
    pub(super) fn worn(&self, window: WindowId, rings: &Rings) -> Worn {
        Worn {
            decoration: self.decoration_of(window, rings),
            wears_ring: rings.ring(window),
            holds_focus: rings.focused(window),
        }
    }
}

impl Reactor {
    /// Tells the rest windows a flight began, when the batch handed the engine one, and sends them
    /// their model if it changed since the last one sent, or turns them off once no window can wear a
    /// band, and follows the window server's frames and orders for them while they are on. Once per
    /// batch; the flight first, so its batch's model waits for it to land. Nothing while off.
    pub(super) fn publish_rest(&mut self) {
        let flight = self.rest.flight.take();
        let Some(tx) = &self.rest.tx else { return };
        let model = self.config.settings.decorations.draws_bands().then(|| self.rest_model());
        self.rest.following.set(model.is_some());
        if let Some((display, pressed)) = flight.filter(|_| model.is_some()) {
            tx.send(RestEvent::Flight { display, pressed });
        }
        if self.rest.sent == model {
            return;
        }
        tx.send(model.clone().map_or(RestEvent::Off, RestEvent::Model));
        self.rest.sent = model;
    }

    /// Every managed window on a space rini manages, the workspace it is on, what it wears with the
    /// key window holding focus, and what holds its bands.
    pub(super) fn rest_model(&self) -> RestModel {
        let screens = &self.space_state.screens;
        let displays = screens
            .iter()
            .map(|screen| RestDisplay { bounds: ScreenRect::from_cg(screen.bounds), scale: screen.scale })
            .collect();
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
                    group: Group { space: info.space, workspace: info.workspace_id.data().as_ffi() },
                    decoration: self.decoration_of(window, &rings),
                    focused: rings.focused(window),
                    ring: rings.ring(window),
                    held: Held {
                        parked: !shown_workspace || off_strip,
                        mission_control,
                        fullscreen: affinity.is_known_fullscreen_window(server_id),
                    },
                })
            })
            .collect();
        RestModel { displays, windows }
    }
}
