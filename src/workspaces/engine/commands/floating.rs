//! Maximising a column: `ctrl-F`, and an app zooming its window from the title bar, which means the
//! same and goes the same way.
//!
//! `impl LayoutEngine` across modules is the pattern `engine/persistence/` already uses.

use super::CommandTarget;
use crate::workspaces::domain::display_memory::DisplayMemory;
use crate::workspaces::engine::{EventResponse, LayoutCommand, LayoutEngine};
use crate::workspaces::{LayoutId, VirtualWorkspaceId};
use rini_core::ids::{SpaceId, WindowId};

impl LayoutEngine {
    pub(in crate::workspaces::engine) fn handle_floating_command(
        &mut self,
        memory: &mut DisplayMemory,
        target: CommandTarget,
        command: LayoutCommand,
    ) -> EventResponse {
        let CommandTarget {
            space,
            workspace: workspace_id,
            layout,
        } = target;
        match command {
            LayoutCommand::ToggleFullscreenWithinGaps => {
                match self.workspace_tree(workspace_id).selected_window(layout) {
                    Some(window) => {
                        self.toggle_full_width(memory, space, workspace_id, layout, window)
                    }
                    None => EventResponse::default(),
                }
            }
            other => {
                debug_assert!(false, "{other:?} is not a floating command");
                EventResponse::default()
            }
        }
    }

    /// Maximise `window`, or put it back to the width and stack it had, and remember which per
    /// display.
    pub(in crate::workspaces::engine) fn toggle_full_width(
        &mut self,
        memory: &mut DisplayMemory,
        space: SpaceId,
        workspace_id: VirtualWorkspaceId,
        layout: LayoutId,
        window: WindowId,
    ) -> EventResponse {
        if !self.workspace_tree_mut(workspace_id).toggle_full_width(layout, window) {
            return EventResponse::default();
        }
        self.remember_column_width(memory, space, workspace_id, layout, window);
        EventResponse {
            changed: true,
            raise_windows: vec![window],
            ..EventResponse::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use objc2_core_foundation::{CGPoint, CGRect, CGSize};
    use rustc_hash::FxHashMap as HashMap;

    use super::*;
    use crate::layout::settings::LayoutSettings;
    use crate::windows::domain::info::WindowInfo;
    use crate::windows::domain::state::WindowState;
    use crate::workspaces::domain::display_affinity::ColumnWidth;
    use crate::workspaces::settings::VirtualWorkspaceSettings;
    use crate::workspaces::{LayoutEvent, WindowStore};
    use rini_core::ids::WindowServerId;

    const DISPLAY: &str = "display-a";

    struct Strip {
        engine: LayoutEngine,
        memory: DisplayMemory,
        store: WindowStore,
        space: SpaceId,
        windows: [WindowId; 2],
    }

    impl Strip {
        /// Two columns, the second one selected.
        fn new() -> Self {
            let mut engine =
                LayoutEngine::new(&VirtualWorkspaceSettings::default(), &LayoutSettings::default());
            let mut memory = DisplayMemory::default();
            let mut store = WindowStore::default();
            let space = SpaceId::new(7);
            let _ = engine.handle_event(
                &mut store,
                &mut memory,
                LayoutEvent::SpaceExposed(space, CGSize::new(1440.0, 900.0)),
            );
            engine.update_space_display(&mut memory, space, Some(DISPLAY.to_owned()));
            let windows = [WindowId::new(3, 1), WindowId::new(3, 2)];
            for window in windows {
                let frame = CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(720.0, 900.0));
                store.insert_window(
                    window,
                    WindowState {
                        info: WindowInfo {
                            is_standard: true,
                            is_root: true,
                            is_minimized: false,
                            is_resizable: true,
                            min_size: None,
                            max_size: None,
                            title: String::new(),
                            frame,
                            sys_id: Some(WindowServerId::new(window.idx.get())),
                            bundle_id: None,
                            path: None,
                            ax_role: None,
                            ax_subrole: None,
                            is_modal: false,
                        },
                        frame_monotonic: frame,
                        is_manageable: true,
                        ignore_app_rule: false,
                    },
                );
                let _ = engine.handle_event(
                    &mut store,
                    &mut memory,
                    LayoutEvent::WindowAdded(space, window),
                );
            }
            let _ = engine.handle_event(
                &mut store,
                &mut memory,
                LayoutEvent::WindowFocused(space, windows[1]),
            );
            Strip {
                engine,
                memory,
                store,
                space,
                windows,
            }
        }

        fn zoomed(&mut self, window: WindowId) -> EventResponse {
            self.engine
                .handle_event(
                    &mut self.store,
                    &mut self.memory,
                    LayoutEvent::WindowZoomed(self.space, window),
                )
                .response
        }

        fn ctrl_f(&mut self) -> EventResponse {
            self.engine.handle_command(
                &mut self.store,
                &mut self.memory,
                Some(self.space),
                &[self.space],
                &HashMap::default(),
                LayoutCommand::ToggleFullscreenWithinGaps,
            )
        }

        fn full_width(&self, window: WindowId) -> Option<bool> {
            self.engine.full_width_of_column(self.space, window)
        }

        fn remembered(&self, window: WindowId) -> Option<ColumnWidth> {
            self.memory.affinity.window_width(DISPLAY, window)
        }
    }

    #[test]
    fn a_zoom_maximises_the_window_it_names_and_remembers_it_for_the_display() {
        let mut strip = Strip::new();
        let [zoomed, selected] = strip.windows;

        let response = strip.zoomed(zoomed);
        assert!(response.changed);
        assert_eq!(response.raise_windows, vec![zoomed]);
        assert_eq!(strip.full_width(zoomed), Some(true));
        assert_eq!(strip.full_width(selected), Some(false));
        assert_eq!(strip.remembered(zoomed), Some(ColumnWidth::FullWidth));

        assert!(strip.zoomed(zoomed).changed);
        assert_eq!(strip.full_width(zoomed), Some(false));
        assert_eq!(
            strip.remembered(zoomed),
            None,
            "back to the display's default width"
        );
    }

    #[test]
    fn ctrl_f_and_a_zoom_are_the_same_toggle() {
        let mut strip = Strip::new();
        let [_, selected] = strip.windows;

        assert_eq!(strip.ctrl_f().raise_windows, vec![selected]);
        assert_eq!(strip.full_width(selected), Some(true));
        assert!(strip.zoomed(selected).changed);
        assert_eq!(
            strip.full_width(selected),
            Some(false),
            "the zoom takes ctrl-F back out"
        );
        assert!(strip.zoomed(selected).changed);
        assert!(strip.ctrl_f().changed);
        assert_eq!(strip.full_width(selected), Some(false), "and ctrl-F a zoom");
    }

    #[test]
    fn a_zoom_of_a_floating_window_changes_nothing() {
        let mut strip = Strip::new();
        let [_, selected] = strip.windows;
        let _ = strip.engine.handle_command(
            &mut strip.store,
            &mut strip.memory,
            Some(strip.space),
            &[strip.space],
            &HashMap::default(),
            LayoutCommand::ToggleWindowFloating,
        );
        assert!(strip.engine.is_window_floating(selected));

        assert_eq!(strip.zoomed(selected), EventResponse::default());
        assert_eq!(strip.full_width(selected), None);
        assert_eq!(strip.remembered(selected), None);
    }
}
