//! An application's width on a display shape, as the engine writes it and gives it to new windows.

use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use rini_core::ids::{WindowServerId, pid_t};

use super::*;
use crate::layout::LayoutId;
use crate::layout::settings::LayoutSettings;
use crate::windows::domain::info::{AppInfo, WindowInfo};
use crate::windows::domain::rules::{RuleMatch, WindowRule};
use crate::windows::domain::state::WindowState;
use crate::workspaces::domain::display_affinity::Removal;
use crate::workspaces::domain::display_setup::SetupId;
use crate::workspaces::domain::launch_memory::Slot;
use crate::workspaces::domain::virtual_workspace::VirtualWorkspaceId;
use crate::workspaces::engine::{AppWindows, LayoutEvent, on_screen_entry};
use crate::workspaces::settings::VirtualWorkspaceSettings;
use crate::workspaces::{EngineCommand, LayoutCommand};

const GHOSTTY: &str = "com.mitchellh.ghostty";
const BUILT_IN: &str = "37D8832A-2D66-02CA-B9F7-8F30A301B230";

/// The 16" built-in alone: 1728x1085 left for windows, 34cm wide.
const SIXTEEN: DisplayShape = DisplayShape {
    width_pt: 1728,
    height_pt: 1085,
    width_cm: 34,
};

/// Kai's `layout` block (`~/.config/rini/config.kdl`, 2026-11-04): a half by default, a third to two
/// thirds, presets a third, a half, two thirds and the whole.
fn kais_layout() -> LayoutSettings {
    let mut settings = LayoutSettings::default();
    settings.scrolling.column_width_ratio = 0.5;
    settings.scrolling.min_column_width_ratio = 0.33333;
    settings.scrolling.max_column_width_ratio = 0.66667;
    settings.scrolling.preset_column_widths = vec![0.33333, 0.5, 0.66667, 1.0];
    settings
}

/// One display, its space exposed, with the window store and the machine's memory rini drives the
/// engine with.
struct Desk {
    engine: LayoutEngine,
    windows: WindowStore,
    memory: DisplayMemory,
    space: SpaceId,
    display: DisplayUuid,
}

impl Desk {
    fn new(display: &str, shape: DisplayShape) -> Self {
        Self::with(&VirtualWorkspaceSettings::default(), display, shape)
    }

    fn with(settings: &VirtualWorkspaceSettings, display: &str, shape: DisplayShape) -> Self {
        let mut desk = Self {
            engine: LayoutEngine::new(settings, &kais_layout()),
            windows: WindowStore::default(),
            memory: DisplayMemory::default(),
            space: SpaceId::new(11),
            display: DisplayUuid::new(display),
        };
        desk.attach(desk.space, display, shape);
        desk
    }

    /// `display`, of `shape`, attached alone and showing `space`, as rini starts at another desk.
    fn attach(&mut self, space: SpaceId, display: &str, shape: DisplayShape) {
        let size = CGSize::new(f64::from(shape.width_pt), f64::from(shape.height_pt));
        let _ = self.engine.handle_event(
            &mut self.windows,
            &mut self.memory,
            LayoutEvent::SpaceExposed(space, size),
        );
        let display = DisplayUuid::new(display);
        self.engine.update_space_display(&mut self.memory, space, Some(display.clone()));
        self.engine.set_connected_displays(vec![display.clone()]);
        self.engine.set_display_shapes([(display.clone(), shape)]);
        self.space = space;
        self.display = display;
    }

    /// A window of `app` titled `title` opens, and the app reports all its windows, as it does.
    fn open(&mut self, window: WindowId, app: &str, title: &str) {
        self.store(window, app, title);
        self.report(window.pid, app);
    }

    /// The reactor adds `window` to the layout, before its app has reported it.
    fn add(&mut self, window: WindowId) {
        let _ = self.engine.handle_event(
            &mut self.windows,
            &mut self.memory,
            LayoutEvent::WindowAdded(self.space, window),
        );
    }

    /// A window of `app` titled `title` in the window store, as the reactor puts it there.
    fn store(&mut self, window: WindowId, app: &str, title: &str) {
        let frame = CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(800.0, 600.0));
        self.windows.insert_window(window, WindowState {
            info: WindowInfo {
                is_standard: true,
                is_root: true,
                is_minimized: false,
                is_resizable: true,
                min_size: None,
                max_size: None,
                title: title.into(),
                frame,
                sys_id: Some(WindowServerId::new(window.idx.get())),
                bundle_id: Some(app.into()),
                path: None,
                ax_role: None,
                ax_subrole: None,
                is_modal: false,
            },
            frame_monotonic: frame,
            is_manageable: true,
            ignore_app_rule: false,
        });
    }

    /// The app of `pid` reports every window it has.
    fn report(&mut self, pid: pid_t, app: &str) {
        let mut ids: Vec<WindowId> = self
            .windows
            .iter_windows()
            .map(|(id, _)| id)
            .filter(|id| id.pid == pid)
            .collect();
        ids.sort_unstable();
        let windows =
            ids.into_iter().map(|id| on_screen_entry(id, self.windows.window(id))).collect();
        let _ = self.engine.handle_event(
            &mut self.windows,
            &mut self.memory,
            LayoutEvent::WindowsOnScreenUpdated(self.space, AppWindows {
                pid,
                windows,
                app_info: Some(AppInfo {
                    bundle_id: Some(app.into()),
                    localized_name: None,
                }),
            }),
        );
    }

    fn command(&mut self, command: LayoutCommand) {
        let _ = self.engine.handle_command(
            &mut self.windows,
            &mut self.memory,
            Some(self.space),
            &[self.space],
            &Default::default(),
            EngineCommand::from(command),
        );
    }

    /// `window` focused and its column put through the preset cycle `presses` times: from the
    /// default half, once is two thirds.
    fn cycle(&mut self, window: WindowId, presses: usize) {
        let _ = self.engine.handle_event(
            &mut self.windows,
            &mut self.memory,
            LayoutEvent::WindowFocused(self.space, window),
        );
        for _ in 0..presses {
            self.command(LayoutCommand::CyclePresetColumnWidth);
        }
    }

    fn laid(&self, window: WindowId) -> (VirtualWorkspaceId, LayoutId) {
        let workspace = self
            .engine
            .virtual_workspace_manager
            .workspace_for_window(&self.windows, self.space, window)
            .expect("the window has a workspace");
        let layout = self.engine.workspace_layouts.active(self.space, workspace).expect("a layout");
        (workspace, layout)
    }

    /// The width `window`'s column has, `None` at the default.
    fn width(&self, window: WindowId) -> Option<ColumnWidth> {
        let (workspace, layout) = self.laid(window);
        let tree = self.engine.workspace_tree(workspace);
        assert!(
            tree.contains_window(layout, window),
            "{window:?} is in its strip"
        );
        tree.carried_width(layout, window)
    }

    fn close(&mut self, window: WindowId) {
        self.windows.remove_window(window);
        let _ = self.engine.handle_event(
            &mut self.windows,
            &mut self.memory,
            LayoutEvent::WindowRemoved(window, Removal::Closed),
        );
    }
}

/// Two thirds, as the preset cycle gives it from the default half: an offset of a sixth from it.
fn two_thirds() -> Option<ColumnWidth> { Some(ColumnWidth::Offset(0.66667 - 0.5)) }

/// Kai's report, failure 1: Cmd+N in a terminal he had made two thirds wide opened the new window
/// at the default half. It opens at the app's last width on this display.
#[test]
fn a_new_window_of_a_running_app_opens_at_the_width_its_app_last_had_on_that_display() {
    let mut desk = Desk::new(BUILT_IN, SIXTEEN);
    let first = WindowId::new(400, 1);
    desk.open(first, GHOSTTY, "~/projects/rini");
    desk.cycle(first, 1);
    assert_eq!(desk.width(first), two_thirds());

    let second = WindowId::new(400, 2);
    desk.open(second, GHOSTTY, "~");
    assert_eq!(desk.width(second), two_thirds(), "the app's last width");
    assert_eq!(desk.width(first), two_thirds(), "the first keeps its own");
}

/// Kai's answer 2: an app's width carries to an arrangement not seen before, on a display of the
/// same shape. The 16" lid closed at one desk is a display of another UUID than the same model at
/// another, and the file rini wrote at the first is the one it starts with at the second.
#[test]
fn an_app_opened_at_a_desk_not_seen_before_on_a_display_of_the_same_shape_takes_its_width() {
    let mut desk = Desk::new(BUILT_IN, SIXTEEN);
    let before = WindowId::new(400, 1);
    desk.open(before, GHOSTTY, "~/projects/rini");
    desk.cycle(before, 1);
    let (_dir, path) = layout_file();
    desk.engine.save(&desk.memory, path.clone()).expect("the file is written");

    let (_, memory) = LayoutEngine::load_for_startup_restore(path, |_, _| false, |_| false)
        .expect("the file loads");
    let mut elsewhere = Desk::new("another-desk-of-one-model", SIXTEEN);
    elsewhere.memory = memory;
    elsewhere.attach(SpaceId::new(23), "another-desk-of-one-model", SIXTEEN);
    let after = WindowId::new(901, 1);
    elsewhere.open(after, GHOSTTY, "~/projects/rini");
    assert_eq!(elsewhere.width(after), two_thirds());
}

/// A display of another shape is not this one: the app opens there at the default until it is
/// given a width there.
#[test]
fn an_app_opened_on_a_display_of_another_shape_opens_at_the_default() {
    let mut desk = Desk::new(BUILT_IN, SIXTEEN);
    let before = WindowId::new(400, 1);
    desk.open(before, GHOSTTY, "~/projects/rini");
    desk.cycle(before, 1);
    let memory = std::mem::take(&mut desk.memory);

    let thirty_two = DisplayShape {
        width_pt: 3008,
        height_pt: 1660,
        width_cm: 70,
    };
    let mut docked = Desk::new("the-32", thirty_two);
    docked.memory = memory;
    docked.attach(SpaceId::new(23), "the-32", thirty_two);
    let after = WindowId::new(901, 1);
    docked.open(after, GHOSTTY, "~/projects/rini");
    assert_eq!(docked.width(after), None);
}

/// A width chosen under one `default-column-width` is not the user's answer to another: after the
/// default is edited a new window opens at the new default, and the old width is dropped.
#[test]
fn an_edited_default_column_width_is_not_beaten_by_a_width_chosen_under_the_old_one() {
    let mut desk = Desk::new(BUILT_IN, SIXTEEN);
    let first = WindowId::new(400, 1);
    desk.open(first, GHOSTTY, "~/projects/rini");
    desk.cycle(first, 1);

    let mut edited = kais_layout();
    edited.scrolling.column_width_ratio = 0.6;
    desk.engine.set_layout_settings(&edited);
    let second = WindowId::new(400, 2);
    desk.open(second, GHOSTTY, "~");
    assert_eq!(desk.width(second), None, "the edited default");

    desk.engine.forget_app_widths_under_another_default(&mut desk.memory);
    assert!(
        desk.memory.app_widths.is_empty(),
        "the width made under 0.5 is dropped"
    );
}

/// A window the window server takes away and gives back is not new: it keeps the default it had,
/// rather than taking the width a sibling was given since.
#[test]
fn a_window_given_back_at_the_default_does_not_take_its_app_s_width() {
    let mut desk = Desk::new(BUILT_IN, SIXTEEN);
    let untouched = WindowId::new(400, 1);
    let widened = WindowId::new(400, 2);
    desk.open(untouched, GHOSTTY, "~");
    desk.open(widened, GHOSTTY, "~/projects/rini");
    desk.cycle(widened, 1);

    let _ = desk.engine.handle_event(
        &mut desk.windows,
        &mut desk.memory,
        LayoutEvent::WindowRemoved(untouched, Removal::Minimized),
    );
    desk.report(400, GHOSTTY);
    assert_eq!(desk.width(untouched), None);
}

/// A window the reactor adds before its app reports it takes its app's width as one the app
/// reports does, and one given back that way keeps the default it had.
#[test]
fn a_window_added_before_its_app_reports_it_takes_its_app_s_width_once() {
    let mut desk = Desk::new(BUILT_IN, SIXTEEN);
    let first = WindowId::new(400, 1);
    let untouched = WindowId::new(400, 2);
    desk.open(first, GHOSTTY, "~/projects/rini");
    desk.store(untouched, GHOSTTY, "~/notes");
    desk.add(untouched);
    desk.cycle(first, 1);

    let added = WindowId::new(400, 3);
    desk.store(added, GHOSTTY, "~");
    desk.add(added);
    assert_eq!(desk.width(added), two_thirds());

    let _ = desk.engine.handle_event(
        &mut desk.windows,
        &mut desk.memory,
        LayoutEvent::WindowRemoved(untouched, Removal::Minimized),
    );
    desk.add(untouched);
    assert_eq!(desk.width(untouched), None, "the default it had");
}

/// A width put back at the default is the app's last width: the next window opens at the default.
#[test]
fn a_window_put_back_at_the_default_leaves_its_app_at_the_default() {
    let mut desk = Desk::new(BUILT_IN, SIXTEEN);
    let first = WindowId::new(400, 1);
    desk.open(first, GHOSTTY, "~/projects/rini");
    desk.cycle(first, 1);
    desk.command(LayoutCommand::ToggleFullscreenWithinGaps);
    assert_eq!(desk.width(first), Some(ColumnWidth::FullWidth));
    let second = WindowId::new(400, 2);
    desk.open(second, GHOSTTY, "~");
    assert_eq!(desk.width(second), Some(ColumnWidth::FullWidth));

    desk.cycle(second, 0);
    desk.command(LayoutCommand::ToggleFullscreenWithinGaps);
    assert_eq!(desk.width(second), None, "back at the default it opened at");
    let third = WindowId::new(400, 3);
    desk.open(third, GHOSTTY, "~/notes");
    assert_eq!(desk.width(third), None);
}

/// The resize keys give the app its width as the preset cycle does.
#[test]
fn a_width_the_resize_keys_give_is_its_app_s_last_width() {
    let mut desk = Desk::new(BUILT_IN, SIXTEEN);
    let first = WindowId::new(400, 1);
    desk.open(first, GHOSTTY, "~/projects/rini");
    desk.cycle(first, 0);
    desk.command(LayoutCommand::ResizeWindowBy {
        amount: 0.125,
        orientation: crate::layout::ResizeOrientation::Horizontal,
    });
    let widened = desk.width(first);
    assert_eq!(widened, Some(ColumnWidth::Offset(0.125)));

    let second = WindowId::new(400, 2);
    desk.open(second, GHOSTTY, "~");
    assert_eq!(desk.width(second), widened);
}

/// A height key changes no width, so it leaves the app's width as it was: a stacked column at the
/// default had written the default as the app's, and the next window opened at a half.
#[test]
fn a_height_key_in_a_column_at_the_default_leaves_its_app_s_width() {
    use crate::layout::{Direction, ResizeOrientation};

    for orientation in [ResizeOrientation::Vertical, ResizeOrientation::Smart] {
        let mut desk = Desk::new(BUILT_IN, SIXTEEN);
        let (top, bottom, widened) = (
            WindowId::new(400, 1),
            WindowId::new(400, 2),
            WindowId::new(400, 3),
        );
        desk.open(top, GHOSTTY, "~");
        desk.open(bottom, GHOSTTY, "~/notes");
        desk.cycle(bottom, 0);
        desk.command(LayoutCommand::ToggleFold(Direction::Left));
        desk.open(widened, GHOSTTY, "~/projects/rini");
        desk.cycle(widened, 1);
        assert_eq!(
            desk.width(bottom),
            None,
            "{orientation:?}: the stack is at the default"
        );

        desk.cycle(bottom, 0);
        desk.command(LayoutCommand::ResizeWindowBy { amount: 0.125, orientation });
        assert_eq!(desk.width(bottom), None, "{orientation:?}");
        let next = WindowId::new(400, 4);
        desk.open(next, GHOSTTY, "~");
        assert_eq!(desk.width(next), two_thirds(), "{orientation:?}");
    }
}

/// A width set by dragging an edge is the app's, as a key's is.
#[test]
fn a_dragged_width_is_its_app_s_last_width() {
    let mut desk = Desk::new(BUILT_IN, SIXTEEN);
    let first = WindowId::new(400, 1);
    desk.open(first, GHOSTTY, "~/projects/rini");
    let (workspace, layout) = desk.laid(first);
    let dragged = ColumnWidth::Unbounded(0.33797 - 0.5);
    desk.engine
        .workspace_tree_mut(workspace)
        .set_carried_width(layout, first, dragged);
    desk.engine.remember_dragged_width(&desk.windows, &mut desk.memory, first);

    let second = WindowId::new(400, 2);
    desk.open(second, GHOSTTY, "~");
    assert_eq!(desk.width(second), Some(dragged));
}

/// A relaunched window's own slot is its width, the default among them: the app's width is for a
/// window no slot holds.
#[test]
fn a_relaunched_window_s_slot_beats_its_app_s_width() {
    let mut desk = Desk::new(BUILT_IN, SIXTEEN);
    let before = WindowId::new(400, 1);
    desk.open(before, GHOSTTY, "~/projects/rini");
    desk.engine
        .remember_launch_slots(&desk.windows, &mut desk.memory, &[desk.display.clone()]);
    let other = WindowId::new(400, 2);
    desk.open(other, GHOSTTY, "~");
    desk.cycle(other, 1);
    desk.close(before);
    desk.close(other);
    let _ =
        desk.engine
            .handle_event(&mut desk.windows, &mut desk.memory, LayoutEvent::AppClosed(400));
    let slots = desk.memory.launch.slots(GHOSTTY, &SetupId::of([&desk.display]));
    assert_eq!(slots.iter().map(|slot: &Slot| slot.width).collect::<Vec<_>>(), [
        None
    ]);

    let after = WindowId::new(901, 1);
    desk.open(after, GHOSTTY, "~/projects/rini");
    assert_eq!(
        desk.width(after),
        None,
        "the slot's default, not the app's two thirds"
    );
    let beyond = WindowId::new(901, 2);
    desk.open(beyond, GHOSTTY, "~");
    assert_eq!(desk.width(beyond), two_thirds(), "no slot is left for it");
}

/// A rule's width is the window's first, so the rule, not the app's width, sizes a window it
/// matches, and a width given to a window a rule sized is not the app's.
#[test]
fn a_window_a_rule_sizes_neither_takes_nor_leaves_its_app_s_width() {
    let settings = VirtualWorkspaceSettings {
        app_rules: vec![WindowRule {
            default_column_width: Some(600.0),
            ..WindowRule::when(RuleMatch::title("rule-sized"))
        }],
        ..VirtualWorkspaceSettings::default()
    };
    let mut desk = Desk::with(&settings, BUILT_IN, SIXTEEN);
    let sized = WindowId::new(400, 1);
    desk.open(sized, GHOSTTY, "rule-sized");
    desk.cycle(sized, 1);
    assert!(
        desk.memory.app_widths.is_empty(),
        "the rule's window leaves nothing"
    );

    let plain = WindowId::new(400, 2);
    desk.open(plain, GHOSTTY, "~");
    desk.cycle(plain, 1);
    let ruled = WindowId::new(400, 3);
    desk.open(ruled, GHOSTTY, "rule-sized");
    assert_eq!(desk.width(ruled), None, "nothing but the rule's resize sizes it");
}

/// A focuslab window is a fixture of a screen test: it leaves no width for Kai's terminals and
/// takes none.
#[test]
fn a_focuslab_window_neither_leaves_nor_takes_an_app_width() {
    let mut desk = Desk::new(BUILT_IN, SIXTEEN);
    let lab = WindowId::new(400, 1);
    desk.open(lab, GHOSTTY, "lab-A");
    desk.cycle(lab, 1);
    assert!(desk.memory.app_widths.is_empty());

    let terminal = WindowId::new(400, 2);
    desk.open(terminal, GHOSTTY, "~");
    desk.cycle(terminal, 1);
    let next_lab = WindowId::new(400, 3);
    desk.open(next_lab, GHOSTTY, "lab-B");
    assert_eq!(desk.width(next_lab), None);
}

/// A width handed to a new window after it took its app's, as a window back from native
/// fullscreen under a new id is handed its old id's, wins over the app's.
#[test]
fn a_width_handed_to_a_new_window_beats_its_app_s() {
    let mut desk = Desk::new(BUILT_IN, SIXTEEN);
    let old = WindowId::new(400, 1);
    let sibling = WindowId::new(400, 2);
    desk.open(old, GHOSTTY, "~/projects/rini");
    desk.cycle(old, 0);
    desk.command(LayoutCommand::ToggleFullscreenWithinGaps);
    desk.open(sibling, GHOSTTY, "~");
    assert_eq!(desk.width(sibling), Some(ColumnWidth::FullWidth), "the app's");
    desk.cycle(sibling, 1);
    let given = desk.width(sibling);
    assert_ne!(given, Some(ColumnWidth::FullWidth));

    let owner = WindowId::new(400, 3);
    desk.open(owner, GHOSTTY, "~/projects/rini");
    assert_eq!(desk.width(owner), given, "the app's");
    let _ = desk.engine.handle_event(
        &mut desk.windows,
        &mut desk.memory,
        LayoutEvent::WindowRemoved(old, Removal::OwnedByAnotherId(owner)),
    );
    assert_eq!(desk.width(owner), Some(ColumnWidth::FullWidth), "its old id's");
}

/// The app's width is for the display the window was first seen on: arriving on another, the
/// window takes none, since that display's shape may want another.
#[test]
fn a_new_window_arriving_on_another_display_takes_no_app_width() {
    let mut desk = Desk::new(BUILT_IN, SIXTEEN);
    let first = WindowId::new(400, 1);
    desk.open(first, GHOSTTY, "~/projects/rini");
    desk.cycle(first, 1);
    let new = NewWindow {
        id: WindowId::new(400, 2),
        space: desk.space,
        app_id: GHOSTTY,
        title: Some("~"),
    };
    desk.engine.take_app_width(&desk.memory, new);
    assert_eq!(
        desk.engine.take_arrival_width(new.id, &DisplayUuid::new("the-32")),
        None
    );
    assert_eq!(
        desk.engine.take_arrival_width(new.id, &desk.display),
        None,
        "taken once"
    );

    desk.engine.take_app_width(&desk.memory, new);
    assert_eq!(
        desk.engine.take_arrival_width(new.id, &desk.display),
        two_thirds()
    );
}

/// A closed window's id, and every id of an app that quit, is forgotten, so the set of windows
/// seen holds only live ones.
#[test]
fn a_closed_window_and_a_quit_app_s_windows_are_forgotten_as_seen() {
    let mut desk = Desk::new(BUILT_IN, SIXTEEN);
    let (closed, kept, other_app) =
        (WindowId::new(400, 1), WindowId::new(400, 2), WindowId::new(7, 1));
    desk.open(closed, GHOSTTY, "~");
    desk.open(kept, GHOSTTY, "~/projects/rini");
    desk.open(other_app, "com.apple.Safari", "Start Page");
    desk.close(closed);
    assert!(!desk.engine.app_width_runtime.seen.contains(&closed));
    let _ =
        desk.engine
            .handle_event(&mut desk.windows, &mut desk.memory, LayoutEvent::AppClosed(400));
    assert!(!desk.engine.app_width_runtime.seen.contains(&kept));
    assert!(desk.engine.app_width_runtime.seen.contains(&other_app));
}

fn layout_file() -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().expect("a temporary directory");
    let path = dir.path().join("layout.ron");
    (dir, path)
}
