//! The reactor's half of the bar: what each bar is told, and what a click on one asks for.
//!
//! After every batch of events the reactor fills a `BarInput` from its own stores and sends the model
//! it makes only when that differs from the last one sent, so a burst that changes nothing on the bar
//! costs the bar nothing. See `src/bar/docs/README.md`.

use rini_ipc::protocol::LayoutCommand;

use crate::bar::domain::model::{
    self, Action, BarInput, BarModel, DisplayInput, FocusInput, WindowInput,
};
use crate::bar::platform::actor::{Event as BarEvent, Sender};

use super::Reactor;
use super::events::EventOutcome;

/// Where the bars are drawn, and the model they were last sent.
#[derive(Default)]
pub(super) struct BarFeed {
    /// `None` until the main thread's bars are wired in, and in tests that do not look.
    pub(super) tx: Option<Sender>,
    sent: Option<BarModel>,
}

impl Reactor {
    /// Sends the bars their model if it changed since the last one sent. Once per batch of events.
    pub(super) fn publish_bar(&mut self) {
        let Some(tx) = &self.bar.tx else { return };
        let model = if self.config.settings.bar.enabled {
            model::build(&self.bar_input())
        } else {
            BarModel::default()
        };
        if self.bar.sent.as_ref() == Some(&model) {
            return;
        }
        tx.send(BarEvent::Model(model.clone()));
        self.bar.sent = Some(model);
    }

    /// The machine woke, so the time the bars show is read again rather than at the next minute.
    pub(super) fn bar_clock_changed(&self) {
        if let Some(tx) = &self.bar.tx {
            tx.send(BarEvent::ClockChanged);
        }
    }

    /// Everything the bars are built from, read off the stores and nothing else, so building it asks
    /// the window server for nothing.
    ///
    /// A display showing no user space (a native fullscreen one) has no bar. Its windows are listed in
    /// the order `query diagnostics` lists them.
    pub(super) fn bar_input(&self) -> BarInput {
        let workspaces = self.layout_manager.layout_engine.virtual_workspace_manager();
        let displays = self
            .space_state
            .screens
            .iter()
            .filter_map(|screen| {
                let space = screen.space?;
                let occupied = workspaces
                    .existing_workspaces(space)
                    .into_iter()
                    .map(|(workspace, _)| {
                        !workspaces
                            .workspace_windows(&self.state.windows, space, workspace)
                            .is_empty()
                    })
                    .collect();
                let windows = workspaces
                    .windows_in_active_workspace(&self.state.windows, space)
                    .into_iter()
                    .map(|window| WindowInput {
                        window,
                        app: self.app_name(window),
                    })
                    .collect();
                Some(DisplayInput {
                    uuid: screen.display_uuid.clone(),
                    screen: screen.id.as_u32(),
                    occupied,
                    shown: workspaces.active_workspace_idx(space).map(|index| index as usize),
                    windows,
                })
            })
            .collect();
        BarInput {
            displays,
            focus: self.bar_focus(),
        }
    }

    /// The focused window and the display it is on: the one its workspace belongs to, else the one its
    /// frame is on.
    fn bar_focus(&self) -> Option<FocusInput> {
        let window = self.main_window()?;
        let state = self.state.windows.window(window)?;
        let affinity = self.affinity();
        let space = affinity
            .assigned_space_for_window_id(window)
            .or_else(|| affinity.best_space_for_frame(&state.frame_monotonic))?;
        let screen = self.space_state.screen_by_space(space)?;
        Some(FocusInput {
            display: screen.display_uuid.clone(),
            window,
            app: self.app_name(window),
            title: state.info.title.clone(),
        })
    }

    /// A click on a bar.
    ///
    /// A numeral switches the display it is drawn on, named by its UUID and resolved to the space that
    /// display shows now, rather than whichever display has focus. Focusing that display first and then
    /// switching would race the focus change. A display rini does not know or does not manage is
    /// ignored.
    pub(super) fn on_bar_action(&mut self, action: Action) -> anyhow::Result<EventOutcome> {
        match action {
            Action::ShowWorkspace { display, index } => {
                let space = self
                    .space_state
                    .screens
                    .iter()
                    .find(|screen| screen.display_uuid == display)
                    .and_then(|screen| screen.space)
                    .filter(|space| self.is_space_active(*space));
                match space {
                    Some(space) => self
                        .on_layout_command_in(LayoutCommand::SwitchToWorkspace(index), Some(space)),
                    None => Ok(EventOutcome::no_change()),
                }
            }
            Action::Focus(window) => Ok(self.focus_window_anywhere(window)),
        }
    }
}
