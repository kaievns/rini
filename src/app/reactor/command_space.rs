//! Which space and screen a command acts on: the command space, the active display's space, the
//! space of the focused or main window, and the screen a display selector names.

use objc2_core_foundation::CGPoint;
use rini_core::ids::{SpaceId, WindowId, pid_t};

use super::{DisplaySelector, Reactor, ScreenInfo};

impl Reactor {
    /// Resolves rather than caches. Three callers, each once per event, and resolving reads the
    /// window server — so this is a real cost but not a hot one, and a cache here would be a second
    /// copy of the answer `space_resolution` exists to give.
    pub(super) fn main_window_space(&self) -> Option<SpaceId> {
        let wid = self.main_window()?;
        self.affinity().best_space_for_window_id(wid)
    }

    /// Window discovery is scoped to one application. It may restore that
    /// application's current focus after its windows have been inserted into
    /// the layout, but it must never replay another application's global main
    /// window. Requiring the command space also prevents a refresh racing an
    /// active-display change from restoring focus on the display being left.
    pub(super) fn focused_window_for_discovery(&self, pid: pid_t) -> Option<(SpaceId, WindowId)> {
        let window = self.main_window().filter(|window| window.pid == pid)?;
        let space = self.main_window_space()?;
        (self.workspace_command_space() == Some(space)).then_some((space, window))
    }

    pub(super) fn raw_command_space(&self) -> Option<SpaceId> { self.space_state.command_space }

    pub(super) fn active_display_space(&self) -> Option<SpaceId> {
        self.raw_command_space()
            .filter(|space| {
                self.space_state.active_spaces.contains(space)
                    && self.space_state.screens.iter().any(|screen| screen.space == Some(*space))
            })
            .or_else(|| {
                self.space_state
                    .screens
                    .iter()
                    .filter_map(|screen| screen.space)
                    .find(|space| self.space_state.active_spaces.contains(space))
            })
    }

    pub(super) fn workspace_command_space(&self) -> Option<SpaceId> {
        self.active_display_space().filter(|space| self.is_space_active(*space))
    }

    pub(super) fn command_context_space(&self) -> Option<SpaceId> {
        self.workspace_command_space().or_else(|| {
            self.layout_manager
                .layout_engine
                .focused_window()
                .and_then(|wid| {
                    self.affinity()
                        .assigned_space_for_window_id(wid)
                        .or_else(|| self.affinity().best_space_for_window_id(wid))
                })
                .filter(|space| self.is_space_active(*space))
                .or_else(|| self.main_window_space().filter(|space| self.is_space_active(*space)))
        })
    }

    pub(super) fn current_screen_center(&self) -> Option<CGPoint> {
        if let Some(space) = self.raw_command_space()
            && let Some(screen) = self.space_state.screen_by_space(space)
        {
            return Some(screen.frame.mid());
        }

        self.space_state.screens.first().map(|screen| screen.frame.mid())
    }

    pub(super) fn screen_for_selector(
        &self,
        selector: &DisplaySelector,
        origin_override: Option<CGPoint>,
    ) -> Option<&ScreenInfo> {
        let origin = origin_override.or_else(|| self.current_screen_center());
        self.space_state.screen_for_selector(selector, origin)
    }
}
