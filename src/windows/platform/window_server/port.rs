//! The window-server port: every query and command a rule or an actor outside `window_server.rs`
//! makes of the window server, behind one trait.
//!
//! A trait object rather than a struct of fn pointers like `SpaceKinds`: the fake keeps answers a
//! test states and the commands it is given, which a fn pointer cannot capture. Consumers hold an
//! `Arc<dyn WindowServer>`, cloned to every thread that asks: production passes [`live`], a test a
//! clone of an `Arc<FakeWindowServer>` it keeps to state answers and read commands. No free
//! function in `window_server` shares a method's name, so nothing reaches the window server around
//! the port.

use std::any::Any;
use std::collections::HashMap;
use std::ffi::c_int;
use std::sync::Arc;

use objc2_core_foundation::{CGPoint, CGRect};
use objc2_core_graphics::CGError;
use rini_core::ids::{SpaceId, WindowId, WindowServerId, pid_t};
use rini_geometry::CGRectExt;

use super::{Listed, depths};
use crate::windows::domain::home_space::home_space;
use crate::windows::domain::info::WindowServerInfo;
use crate::windows::domain::manageable::{Facts, turned_away_before_accessibility};
use crate::windows::domain::stack::{StackPlace, WindowLevel};

/// The window server, as rini asks it and tells it.
///
/// A read answers `None`, or an empty list, when the window server gives no answer. That is no
/// evidence about the window, where `Some(false)` is: a negative answer retires a window, so a read
/// must never guess one. `Any`, so a test reaches the fake it handed over as the port.
pub trait WindowServer: Any + Send + Sync {
    /// `id`, unless the window server has no window by that id or one too faint to be a window.
    fn get_window(&self, id: WindowServerId) -> Option<WindowServerInfo>;
    /// Each of `ids` `get_window` would answer, in one query.
    fn get_windows(&self, ids: &[WindowServerId]) -> Vec<WindowServerInfo>;
    /// The window `id` is a child of. `None` for a top-level window as for no answer.
    fn window_parent(&self, id: WindowServerId) -> Option<WindowServerId>;
    /// Whether `id` is on more than one space.
    fn window_is_sticky(&self, id: WindowServerId) -> Option<bool>;
    fn window_level(&self, id: WindowServerId) -> Option<WindowLevel>;
    fn window_sub_level(&self, id: WindowServerId) -> Option<c_int>;
    /// The spaces the window server places `id` on.
    fn window_spaces(&self, id: WindowServerId) -> Vec<SpaceId>;
    /// Whether `space` is a user space, a Desktop, rather than a fullscreen or a system one.
    fn space_is_user(&self, space: SpaceId) -> Option<bool>;
    /// Whether the window server has `id` ordered in.
    fn window_ordered_in(&self, id: WindowServerId) -> Option<bool>;
    /// Whether `id` is a top-level application window, a document or a floating one.
    fn app_window_suitability(&self, id: WindowServerId) -> Option<bool>;
    /// The app windows on `spaces`, front to back: what callers take as a space's membership.
    fn space_window_list_for_connection(&self, spaces: &[u64]) -> Vec<u32>;
    fn window_under_cursor(&self) -> Option<WindowServerId>;
    /// The topmost window at `point` that is none of rini's own.
    fn get_window_at_point(&self, point: CGPoint) -> Option<WindowServerId>;
    /// The frame the window server has for `id` now, read without asking the app.
    fn server_frame(&self, id: WindowServerId) -> Option<CGRect>;
    /// The corner radius each of `ids` rounds at, for those the window server reports one for.
    fn corner_radii(&self, ids: &[WindowServerId]) -> HashMap<WindowServerId, f64>;
    /// The key window on `space`.
    fn key_focused_window(&self, space: SpaceId) -> Option<WindowId>;
    /// The process holding the keys: the one whose key window is drawn active.
    fn key_focus_pid(&self) -> Option<pid_t>;
    /// Every window on screen, front to back, the desktop's included; with `titles`, each title.
    fn on_screen_windows(&self, titles: bool) -> Vec<Listed>;
    /// Every window the window server has, on screen or not, with titles.
    fn every_window(&self) -> Vec<Listed>;
    /// The desktop windows the window server keeps for the display with `display_uuid`.
    fn desktop_windows(&self, display_uuid: &str) -> Vec<WindowServerId>;

    /// Make `id`, a window of `pid`'s, the key window, as a click on it would.
    fn make_key_window(&self, pid: pid_t, id: WindowServerId) -> Result<(), CGError>;
    /// Put `own`, a window of rini's own connection, at `level` and `sub_level`, as
    /// `stack_place_writes` orders the writes.
    fn take_stack_place(
        &self,
        own: WindowServerId,
        level: Option<WindowLevel>,
        sub_level: Option<c_int>,
    );

    /// The space `id` calls home (`home_space`).
    fn window_space(&self, id: WindowServerId) -> Option<SpaceId> {
        home_space(
            self.window_spaces(id)
                .into_iter()
                .map(|space| (space, self.space_is_user(space))),
        )
    }

    /// The depth of every window on screen but the desktop's, 0 being frontmost.
    fn front_to_back_depths(&self) -> HashMap<u32, usize> { depths(self.on_screen_windows(false)) }

    /// Make the desktop window of `display_uuid` that sits within `frame` the key window, so the
    /// display holds focus with no app window on it. Whether one was made key; none is for a display
    /// with no uuid.
    fn focus_desktop(&self, display_uuid: &str, frame: CGRect) -> bool {
        if display_uuid.is_empty() {
            return false;
        }
        self.desktop_windows(display_uuid).into_iter().any(|id| {
            self.get_window(id).is_some_and(|info| {
                info.layer < 0
                    && frame.contains(info.frame.mid())
                    && self.make_key_window(info.pid, id).is_ok()
            })
        })
    }

    fn corner_radius(&self, id: WindowServerId) -> Option<f64> {
        self.corner_radii(&[id]).get(&id).copied()
    }

    fn stack_level(&self, id: WindowServerId) -> (Option<WindowLevel>, Option<c_int>) {
        (self.window_level(id), self.window_sub_level(id))
    }

    /// `id`'s place in the stack, at `frame`.
    fn stack_place(&self, id: WindowServerId, frame: CGRect) -> StackPlace {
        let (level, sub_level) = self.stack_level(id);
        StackPlace { frame, level, sub_level }
    }

    /// `facts` with the window server's answers about `id` in, `known` the catalogue's record of
    /// `id`, which its layer is read from. Each read is made only while the facts in hand do not
    /// turn the window away (`turned_away_before_accessibility`): none for a minimized window,
    /// neither stickiness nor level for one off layer 0, no level for a sticky one.
    fn with_server_facts(
        &self,
        mut facts: Facts,
        id: WindowServerId,
        known: Option<&WindowServerInfo>,
    ) -> Facts {
        facts.server.layer = known.map(|info| info.layer);
        if !turned_away_before_accessibility(&facts) {
            facts.server.sticky = self.window_is_sticky(id);
        }
        if !turned_away_before_accessibility(&facts) {
            facts.server.level = self.window_level(id);
        }
        facts
    }
}

/// What a command given to the window server was, as a fake records it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    MakeKeyWindow {
        pid: pid_t,
        window: WindowServerId,
    },
    TakeStackPlace {
        own: WindowServerId,
        level: Option<WindowLevel>,
        sub_level: Option<c_int>,
    },
}

/// The window server on this machine, through SkyLight. Outside `window_server`, only [`live`]
/// builds one.
#[derive(Debug, Clone, Copy)]
pub struct LiveWindowServer(pub(super) ());

/// The port production passes: the window server on this machine. A test build panics, since a
/// test's window-server ids can name real windows on the machine running it.
pub fn live() -> Arc<dyn WindowServer> {
    if cfg!(test) {
        panic!("a test passes a FakeWindowServer, never the live window server");
    }
    Arc::new(LiveWindowServer(()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[should_panic(expected = "FakeWindowServer")]
    fn a_test_build_never_takes_the_live_window_server() { drop(live()); }
}
