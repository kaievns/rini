//! An application's width on a display shape: written where the user gives one of its windows a
//! width, given to a window the session has not seen before. See
//! `src/workspaces/docs/launch-memory.md`, "An application's width".

use rini_core::ids::{DisplayUuid, SpaceId, WindowId, pid_t};
use rustc_hash::{FxHashMap as HashMap, FxHashSet as HashSet};
use tracing::{debug, info};

use super::commands::CommandTarget;
use super::rules::NewWindow;
use crate::displays::domain::screen::DisplayShape;
use crate::layout::ColumnWidth;
use crate::workspaces::WindowStore;
use crate::workspaces::domain::app_rules::SlotOverrides;
use crate::workspaces::domain::app_widths::{AppWidth, Overrode};
use crate::workspaces::domain::display_memory::DisplayMemory;
use crate::workspaces::domain::launch_memory::is_focuslab_window;
use crate::workspaces::engine::LayoutEngine;

/// What the engine holds for applications' widths while it runs, none of it saved.
#[derive(Debug, Default)]
pub(super) struct AppWidthRuntime {
    /// The shape of each attached display.
    shapes: HashMap<DisplayUuid, DisplayShape>,
    /// Every window this session has seen. A window among them is not new, so a window the window
    /// server removes and gives back keeps its own width or the default it had, never its app's.
    seen: HashSet<WindowId>,
    /// The width a new window takes from its app when it first joins a strip on the display named.
    /// Held apart from its own record, so a width it is handed later, as a window back from native
    /// fullscreen under a new id is, still wins.
    on_arrival: HashMap<WindowId, (DisplayUuid, ColumnWidth)>,
    /// The app's width each window was laid at on arrival, and on which display, so a window found
    /// later to be one the session has seen, under another id, can be put back at the default.
    laid_from_app: HashMap<WindowId, (DisplayUuid, ColumnWidth)>,
}

impl LayoutEngine {
    /// The shape of each display attached now, which an application's width is kept under.
    pub fn set_display_shapes(
        &mut self,
        shapes: impl IntoIterator<Item = (DisplayUuid, DisplayShape)>,
    ) {
        self.app_width_runtime.shapes = shapes.into_iter().collect();
    }

    /// The default a width given now overrides.
    fn default_overridden(&self) -> Overrode {
        Overrode::DefaultColumnWidth(self.layout_settings.scrolling.column_width_ratio)
    }

    /// Whether this session has not seen `window` before, noting that it has now.
    pub(super) fn first_sighting(&mut self, window: WindowId) -> bool {
        self.app_width_runtime.seen.insert(window)
    }

    /// Whether this session has seen `window`.
    pub(super) fn was_sighted(&self, window: WindowId) -> bool {
        self.app_width_runtime.seen.contains(&window)
    }

    /// Forgets `window`, whose id is dead: closed, or owned by another id now.
    pub(super) fn forget_sighting(&mut self, window: WindowId) {
        self.app_width_runtime.seen.remove(&window);
        self.app_width_runtime.on_arrival.remove(&window);
        self.app_width_runtime.laid_from_app.remove(&window);
    }

    /// Forgets every window of `pid`, which quit.
    pub(super) fn forget_app_sightings(&mut self, pid: pid_t) {
        self.app_width_runtime.seen.retain(|window| window.pid != pid);
        self.app_width_runtime.on_arrival.retain(|window, _| window.pid != pid);
        self.app_width_runtime.laid_from_app.retain(|window, _| window.pid != pid);
    }

    /// Records `width`, `None` for the default, as the one the user last gave `window`'s app on the
    /// display owning `space`. Nothing for a window a rule sized, whose width is the rule's first
    /// (`specs/displays.md`), a focuslab window, or a display whose shape is not known.
    fn remember_app_width(
        &self,
        window_store: &WindowStore,
        memory: &mut DisplayMemory,
        space: SpaceId,
        window: WindowId,
        width: Option<ColumnWidth>,
    ) {
        let Some(state) = window_store.window(window) else {
            return;
        };
        let Some(app_id) = state.info.bundle_id.as_deref() else {
            return;
        };
        if is_focuslab_window(app_id, Some(&state.info.title))
            || window_store.last_rule_size(window).is_some_and(|size| size.w.is_some())
        {
            return;
        }
        let Some(on_display) = memory.affinity.display_for_space(space) else {
            return;
        };
        let Some(&shape) = self.app_width_runtime.shapes.get(on_display) else {
            debug!(%on_display, "no shape known for this display, so no app width is remembered");
            return;
        };
        let overrode = self.default_overridden();
        let Overrode::DefaultColumnWidth(default) = overrode;
        let width = width.map(|width| AppWidth::of(width, default));
        debug!(app_id, ?shape, ?width, "remembering an application's width");
        memory.app_widths.remember(app_id, shape, width, overrode);
    }

    /// Records the width a command just gave `window`'s column, as the window's on its display and
    /// as its app's on the display's shape.
    pub(super) fn remember_given_width(
        &mut self,
        window_store: &WindowStore,
        memory: &mut DisplayMemory,
        target: CommandTarget,
        window: WindowId,
    ) {
        let CommandTarget { space, workspace, layout } = target;
        self.remember_column_width(memory, space, workspace, layout, window);
        let width = self.workspace_tree(workspace).carried_width(layout, window);
        self.remember_app_width(window_store, memory, space, window, width);
    }

    /// Records the width a drag of `window`'s edge left its column at, as the window's and as its
    /// app's.
    pub fn remember_dragged_width(
        &mut self,
        window_store: &WindowStore,
        memory: &mut DisplayMemory,
        window: WindowId,
    ) {
        if let Some((space, width)) = self.remember_width_as_laid(window_store, memory, window) {
            self.remember_app_width(window_store, memory, space, window, Some(width));
        }
    }

    /// Gives `window`, new to the session, the width its app was last given on the shape of the
    /// display owning its space, for when it first joins a strip there with no width of its own.
    pub(super) fn take_app_width(&mut self, memory: &DisplayMemory, window: NewWindow<'_>) {
        if is_focuslab_window(window.app_id, window.title) {
            return;
        }
        let Some(on_display) = memory.affinity.display_for_space(window.space) else {
            return;
        };
        let Some(&shape) = self.app_width_runtime.shapes.get(on_display) else {
            return;
        };
        let overrode = self.default_overridden();
        let Some(width) = memory.app_widths.width(window.app_id, shape, overrode) else {
            debug!(
                idx = window.id.idx.get(),
                app_id = window.app_id,
                ?shape,
                "no width remembered for this application on this display shape"
            );
            return;
        };
        let Overrode::DefaultColumnWidth(default) = overrode;
        info!(
            idx = window.id.idx.get(),
            app_id = window.app_id,
            %on_display,
            ?shape,
            ?width,
            "a new window with no width of its own takes its application's last on this display shape"
        );
        self.app_width_runtime
            .on_arrival
            .insert(window.id, (on_display.clone(), width.column_width(default)));
    }

    /// `take_app_width` for a window no rule has been asked about yet: its rules decide first
    /// whether it takes its app's width, as they do at a first sighting through them.
    pub(super) fn take_app_width_unruled(
        &mut self,
        window_store: &WindowStore,
        memory: &DisplayMemory,
        space: SpaceId,
        window: WindowId,
    ) {
        let Some(state) = window_store.window(window) else {
            return;
        };
        let Some(app_id) = state.info.bundle_id.as_deref() else {
            return;
        };
        let decision = self.evaluate_app_rules(
            Some(app_id),
            None,
            Some(&state.info.title),
            state.info.ax_role.as_deref(),
            state.info.ax_subrole.as_deref(),
            state.info.is_modal,
        );
        if SlotOverrides::of(&decision).is_some_and(|overrides| !overrides.width) {
            self.take_app_width(memory, NewWindow {
                id: window,
                space,
                app_id,
                title: Some(&state.info.title),
            });
        }
    }

    /// The app's width `window` takes as it joins a strip on `display`, once: it is dropped wherever
    /// the window first arrives.
    pub(super) fn take_arrival_width(
        &mut self,
        window: WindowId,
        display: &DisplayUuid,
    ) -> Option<ColumnWidth> {
        let (on, width) = self.app_width_runtime.on_arrival.remove(&window)?;
        (on == *display).then_some(width)
    }

    /// Notes that `window` was laid at its app's `width` on arriving on `display`.
    pub(super) fn note_laid_from_app(
        &mut self,
        window: WindowId,
        display: &DisplayUuid,
        width: ColumnWidth,
    ) {
        self.app_width_runtime.laid_from_app.insert(window, (display.clone(), width));
    }

    /// `owner` now owns a window the session has seen, as the id of a window back from native
    /// fullscreen does: it is not new, so it takes no width from its app, and a width of its app's
    /// it was laid at goes back to the default unless its old id handed it one there.
    pub(super) fn hand_over_sighting(
        &mut self,
        window_store: &WindowStore,
        memory: &DisplayMemory,
        owner: WindowId,
    ) {
        self.app_width_runtime.seen.insert(owner);
        self.app_width_runtime.on_arrival.remove(&owner);
        let Some((on_display, width)) = self.app_width_runtime.laid_from_app.remove(&owner) else {
            return;
        };
        if memory.affinity.window_width(&on_display, owner).is_some() {
            return;
        }
        let Some(assignment) = window_store.workspace_info_for_window(owner) else {
            return;
        };
        let (space, workspace) = (assignment.space, assignment.workspace_id);
        if memory.affinity.display_for_space(space) != Some(&on_display) {
            return;
        }
        let Some(layout) = self.workspace_layouts.active(space, workspace) else {
            return;
        };
        if self.workspace_tree(workspace).carried_width(layout, owner) != Some(width) {
            return;
        }
        info!(
            idx = owner.idx.get(),
            %on_display,
            ?width,
            "a window that owns one seen before goes back to the default its old id had, not its app's width"
        );
        self.workspace_tree_mut(workspace).clear_carried_width(layout, owner);
    }
}

#[cfg(test)]
mod tests;
