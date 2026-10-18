//! The window switcher's session: the list snapshotted when it opens, the cursor stepping over it,
//! the popup drawn and hidden, and the commit that focuses the chosen window wherever it is.

use rini_core::ids::{SpaceId, WindowId};
use rini_geometry::space::{FromCg, ScreenSize};
use rini_ipc::protocol::SwitchScope;
use tracing::{debug, info, warn};

use super::Reactor;
use super::events::{EventOutcome, command};
use crate::input::domain::switch_session::Signal as SwitchSignal;
use crate::switcher::domain::candidates::{Candidate, Scope, opening_selection, switch_list};
use crate::switcher::domain::entries::{Entry, entries};
use crate::switcher::domain::selection::Selection;

/// A switcher session in progress: the list as it was when the switch opened, and the cursor over it.
///
/// Snapshotted at open rather than rebuilt per step, so the list cannot reorder under the user while
/// they are stepping through it — the focus order it is built from would change the moment anything
/// committed.
pub(super) struct LiveSwitch {
    list: Vec<Entry>,
    cursor: Selection,
}

impl Reactor {
    #[cfg(test)]
    pub(crate) fn probe_switch_window(
        &mut self,
        backward: bool,
        scope: rini_ipc::protocol::SwitchScope,
    ) -> EventOutcome {
        self.switch_window(backward, scope)
    }

    #[cfg(test)]
    pub(crate) fn probe_switch_candidates(&mut self, scope: SwitchScope) -> Vec<Candidate> {
        self.switch_scope(scope)
            .map(|scope| self.switch_candidates(scope))
            .unwrap_or_default()
    }

    #[cfg(test)]
    pub(crate) fn probe_switch_entries(&mut self, scope: SwitchScope) -> Vec<Entry> {
        self.switch_scope(scope)
            .map(|scope| self.switch_entries(scope))
            .unwrap_or_default()
    }

    /// Focus `window` wherever it is, switching the owning display's workspace to follow.
    ///
    /// The half of a switch that is not about choosing. One function because a second copy would be a
    /// second set of answers to "what does it take to
    /// reach a window on another workspace": the space resolution, the active-space test that
    /// `handle_command_reactor_focus_window` refuses without, and the workspace follow without which
    /// focus lands on a window parked off-screen and the keystroke looks like a no-op.
    pub(super) fn focus_window_anywhere(&mut self, window: WindowId) -> EventOutcome {
        let resolved_space = self.affinity().best_space_for_window_id(window);
        let space_is_active = resolved_space.is_some_and(|space| self.is_space_active(space));
        match command::handle_command_reactor_focus_window(
            &self.state,
            &self.apps,
            command::FocusWindowPayload {
                window_id: window,
                window_server_id: self
                    .state
                    .windows
                    .window(window)
                    .and_then(|state| state.info.sys_id),
                resolved_space,
                space_is_active,
            },
        ) {
            Ok(mut outcome) => {
                // Switch the owning display to the target's workspace, or focus lands on a
                // window that is parked off-screen and the keystroke looks like a no-op. This
                // is the same follow used by the focus-changed path.
                if let Some(space) = resolved_space {
                    outcome.absorb(
                        self.maybe_auto_switch_to_window_workspace(window.pid, window, space),
                    );
                }
                outcome
            }
            Err(error) => {
                warn!(%error, ?window, "failed to focus window");
                EventOutcome::no_change()
            }
        }
    }

    /// Handle a switcher session signal from the input thread.
    ///
    /// The list and the cursor live HERE rather than in the tap, because the tap answers inside the
    /// event delivery path and must do no work, and because this is the thread the popup will be drawn
    /// on. The input thread holds nothing but a flag and a generation.
    ///
    /// Nothing is focused until the commit. A step that focused as it went would raise every window
    /// the selection passed over, which is both a burst of Accessibility work and a visible flicker
    /// through windows the user never asked to see.
    pub(super) fn handle_switch_signal(&mut self, signal: SwitchSignal) -> EventOutcome {
        match signal {
            SwitchSignal::Open { backward, scope } => {
                // No scope to resolve — an application switch with nothing focused — opens nothing,
                // and the commit that follows finds no live switch and does nothing either.
                let resolved = self.switch_scope(scope);
                let list = resolved.map(|scope| self.switch_entries(scope)).unwrap_or_default();
                // At info, not debug: which window counted as focused and what the scope resolved to
                // are the two facts a wrong list comes down to, and the release build logs at info.
                info!(
                    ?scope,
                    ?resolved,
                    focused = ?self.main_window(),
                    rows = list.len(),
                    windows = list.iter().map(Entry::count).sum::<usize>(),
                    "switch opened"
                );
                let start = opening_selection(&list);
                self.live_switch = start.and_then(|index| {
                    let mut cursor = Selection::new(list.len(), index)?;
                    // `backward` on the opening press means the user wants the other end: the window
                    // before this one in the other direction, which is the last entry rather than the
                    // second.
                    if backward {
                        cursor.step(-2);
                    }
                    Some(LiveSwitch { list, cursor })
                });
                self.draw_switch();
                EventOutcome::no_change()
            }
            SwitchSignal::Step(delta) => {
                if let Some(switch) = self.live_switch.as_mut() {
                    switch.cursor.step(delta);
                }
                self.draw_switch();
                EventOutcome::no_change()
            }
            SwitchSignal::Commit => {
                self.hide_switch();
                let Some(switch) = self.live_switch.take() else {
                    return EventOutcome::no_change();
                };
                let Some(target) = switch.list.get(switch.cursor.index()) else {
                    return EventOutcome::no_change();
                };
                let window = target.front().window;
                if Some(window) == self.main_window() {
                    return EventOutcome::no_change();
                }
                self.focus_window_anywhere(window)
            }
            SwitchSignal::Cancel => {
                self.live_switch = None;
                self.hide_switch();
                EventOutcome::no_change()
            }
            SwitchSignal::Overview(signal) => self.handle_overview_signal(signal),
        }
    }

    /// A click on a popup row: select that row and commit, exactly as releasing the modifier would.
    ///
    /// Through the one cursor every other selection change goes through, by the row's top window
    /// rather than by row index, so a click can never land on a different entry from the one drawn
    /// under the pointer. A window no open row is drawn for commits nothing: guessing would focus
    /// something the user did not point at.
    ///
    /// The tap is told the switch is over, or it would keep swallowing the arrow keys until the modifier
    /// is let go and then send a second commit on that release.
    pub(super) fn pick_switch(&mut self, window: WindowId) -> EventOutcome {
        let Some(switch) = self.live_switch.as_mut() else {
            return EventOutcome::no_change();
        };
        let Some(index) = switch.list.iter().position(|entry| entry.front().window == window)
        else {
            return EventOutcome::no_change();
        };
        switch.cursor.select(index);
        self.end_tap_session();
        self.handle_switch_signal(SwitchSignal::Commit)
    }

    /// Ask the main thread to draw the open switch.
    ///
    /// Silent when there is no popup: a switch works without one, which is what keeps the panel
    /// cosmetic rather than load-bearing.
    fn draw_switch(&self) {
        let Some(tx) = self.switcher_tx.as_ref() else {
            return;
        };
        let Some(switch) = self.live_switch.as_ref() else {
            return;
        };
        let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
        let rows: Vec<crate::switcher::platform::panel::Row> = switch
            .list
            .iter()
            .map(|entry| {
                let icon = self.config.settings.switcher.icon_for(
                    self.apps
                        .get(&entry.front().window.pid)
                        .and_then(|app| app.info.bundle_id.as_deref()),
                    home.as_deref(),
                );
                crate::switcher::platform::panel::Row::of(entry, icon)
            })
            .collect();
        // The display the switch is being driven from, so the popup appears where the user is looking
        // rather than always on the primary.
        let screen = self
            .command_context_space()
            .and_then(|space| self.space_state.screen_by_space(space))
            .or_else(|| self.space_state.screens.first())
            .map(|screen| screen.frame);
        let Some(screen) = screen else {
            return;
        };
        let windows = crate::switcher::platform::panel::drawn_windows(&rows);
        tx.send(crate::switcher::platform::actor::Event::Show {
            rows,
            selected: switch.cursor.index(),
            screen,
        });

        // Queue captures of the cards drawn, so the NEXT open has them, and this one gets those that
        // land while it is up. Background work; nothing is drawn and no window is touched.
        if let Some(pictures) = self.communication_manager.pictures.as_ref() {
            let targets: Vec<crate::pictures::domain::target::PictureTarget> = windows
                .iter()
                .filter_map(|window| {
                    let state = self.state.windows.window(*window)?;
                    Some(crate::pictures::domain::target::PictureTarget {
                        window: *window,
                        server_id: state.info.sys_id?,
                        size: ScreenSize::from_cg(state.frame_monotonic.size),
                    })
                })
                .collect();
            if !targets.is_empty() {
                pictures.ask(crate::pictures::domain::schedule::Ask {
                    priority: crate::pictures::domain::schedule::Priority::Warm,
                    targets,
                });
            }
        }
    }

    fn hide_switch(&self) {
        if let Some(tx) = self.switcher_tx.as_ref() {
            tx.send(crate::switcher::platform::actor::Event::Hide);
        }
    }

    /// Every window a switch over `scope` offers, in its order.
    ///
    /// Built from the workspace assignments rather than from any layout tree: a tree is per display and
    /// per workspace, and this list is deliberately neither. Windows with no workspace assignment are
    /// dropped — there is nowhere to switch TO for a window rini has not placed — and a minimised window
    /// is one: minimising takes it off its workspace.
    ///
    /// The workspace index comes from the canonical order rather than from the workspace id, so the
    /// stable tail matches the order the user sees when they step through workspaces.
    fn switch_candidates(&mut self, scope: Scope) -> Vec<Candidate> {
        let assignments: Vec<(WindowId, SpaceId, crate::workspaces::VirtualWorkspaceId)> = self
            .state
            .windows
            .iter_workspace_assignments()
            .map(|(window, info)| (window, info.space, info.workspace_id))
            .collect();

        let mut candidates = Vec::with_capacity(assignments.len());
        for (window, space, workspace_id) in assignments {
            if !self.window_is_standard(window) {
                continue;
            }
            let workspace_index = self
                .layout_engine
                .virtual_workspace_manager_mut()
                .list_workspaces(space)
                .iter()
                .position(|(id, _)| *id == workspace_id)
                .unwrap_or(usize::MAX);
            let Some(state) = self.state.windows.window(window) else {
                continue;
            };
            let app_name = self.app_name(window);
            candidates.push(Candidate {
                window,
                space,
                workspace: workspace_id,
                workspace_index,
                size: state.frame_monotonic.size,
                title: state.info.title.clone(),
                app_name,
            });
        }

        switch_list(candidates, self.main_window_tracker.focus_order(), scope)
    }

    /// What a switch over `scope` steps between, in its order: windows, or application stacks.
    fn switch_entries(&mut self, scope: Scope) -> Vec<Entry> {
        entries(self.switch_candidates(scope), scope)
    }

    /// What a switch over `scope` covers right now.
    ///
    /// `None` when there is nothing for it to be about: an application switch with no focused window.
    /// A workspace switch with nothing focused takes the workspace on the display the switch is driven
    /// from, because there is always one being looked at. The workspace is a single id on every
    /// display, so the switch spans all of them.
    fn switch_scope(&self, scope: SwitchScope) -> Option<Scope> {
        match scope {
            SwitchScope::Everything => Some(Scope::Everything),
            SwitchScope::App => self.main_window().map(|window| Scope::App(window.pid)),
            SwitchScope::Workspace => self
                .main_window()
                .and_then(|window| self.state.windows.workspace_info_for_window(window))
                .map(|info| info.workspace_id)
                .or_else(|| {
                    let space = self.command_context_space()?;
                    self.layout_engine.active_workspace(space)
                })
                .map(Scope::Workspace),
        }
    }

    /// One step of a switcher over `scope`: focus the next entry's top window, wherever it is.
    ///
    /// The no-popup half of the switcher, and on its own the whole of a quick tap. The same list the
    /// popup draws, so a tap and a held switch can never disagree about what comes next.
    pub(super) fn switch_window(&mut self, backward: bool, scope: SwitchScope) -> EventOutcome {
        let Some(scope) = self.switch_scope(scope) else {
            return EventOutcome::no_change();
        };
        let list = self.switch_entries(scope);
        let current = self.main_window();
        // Every list puts the entry focused now first — unless focus is somewhere rini does not
        // track, in which case the front is still the best guess at "where I was", and stepping from
        // it is right.
        let here = current
            .and_then(|window| list.iter().position(|entry| entry.holds(window)))
            .unwrap_or(0);
        let Some(mut cursor) = Selection::new(list.len(), here) else {
            return EventOutcome::no_change();
        };
        cursor.step(if backward { -1 } else { 1 });
        let target = list[cursor.index()].front().window;
        if Some(target) == current {
            return EventOutcome::no_change();
        }
        debug!(
            idx = target.idx.get(),
            from = ?current.map(|w| w.idx.get()),
            rows = list.len(),
            ?scope,
            "switching window"
        );
        self.focus_window_anywhere(target)
    }
}
