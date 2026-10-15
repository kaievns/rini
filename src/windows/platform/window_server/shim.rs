//! TEMPORARY, for callers not yet handed a [`WindowServer`] port: the free functions they call and
//! the `set_*_override` setters their tests state answers with. Production answers from
//! [`LiveWindowServer`], a test build from a thread-local [`FakeWindowServer`]. The roadmap's
//! P2.2-P2.4 and P2.8 move those callers onto a port and delete this module.

use std::collections::HashMap;
use std::ffi::c_int;

#[cfg(test)]
use objc2_core_foundation::CGSize;
use objc2_core_foundation::{CGPoint, CGRect};
use objc2_core_graphics::CGError;
use rini_core::ids::{SpaceId, WindowId, WindowServerId, pid_t};

#[cfg(test)]
use super::FakeWindowServer;
#[cfg(not(test))]
use super::LiveWindowServer;
use super::{Listed, WindowServer};
use crate::windows::domain::info::WindowServerInfo;
use crate::windows::domain::manageable::Facts;
use crate::windows::domain::stack::{StackPlace, WindowLevel};

#[cfg(test)]
thread_local! {
    static FAKE: FakeWindowServer = FakeWindowServer::default();
}

fn with_port<R>(ask: impl FnOnce(&dyn WindowServer) -> R) -> R {
    #[cfg(test)]
    {
        FAKE.with(|fake| ask(fake))
    }
    #[cfg(not(test))]
    {
        ask(&LiveWindowServer(()))
    }
}

/// What a test build answers for a window no test stated: one at the origin, 800x600, which the
/// tests not yet on the port were written against.
#[cfg(test)]
fn at_origin(id: WindowServerId) -> WindowServerInfo {
    WindowServerInfo {
        id,
        pid: 1234,
        layer: 0,
        frame: CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(800.0, 600.0)),
        min_frame: CGSize::ZERO,
        max_frame: CGSize::ZERO,
    }
}

pub fn get_window(id: WindowServerId) -> Option<WindowServerInfo> {
    #[cfg(test)]
    {
        Some(with_port(|ws| ws.get_window(id)).unwrap_or_else(|| at_origin(id)))
    }
    #[cfg(not(test))]
    with_port(|ws| ws.get_window(id))
}

pub fn get_windows(ids: &[WindowServerId]) -> Vec<WindowServerInfo> {
    #[cfg(test)]
    {
        ids.iter().filter_map(|&id| get_window(id)).collect()
    }
    #[cfg(not(test))]
    with_port(|ws| ws.get_windows(ids))
}

pub fn window_parent(id: WindowServerId) -> Option<WindowServerId> {
    with_port(|ws| ws.window_parent(id))
}

pub fn window_spaces(id: WindowServerId) -> Vec<SpaceId> { with_port(|ws| ws.window_spaces(id)) }

pub fn window_space(id: WindowServerId) -> Option<SpaceId> { with_port(|ws| ws.window_space(id)) }

pub fn window_ordered_in(id: WindowServerId) -> Option<bool> {
    with_port(|ws| ws.window_ordered_in(id))
}

pub fn app_window_suitability(id: WindowServerId) -> Option<bool> {
    with_port(|ws| ws.app_window_suitability(id))
}

pub fn space_window_list_for_connection(spaces: &[u64]) -> Vec<u32> {
    with_port(|ws| ws.space_window_list_for_connection(spaces))
}

pub fn front_to_back_depths() -> HashMap<u32, usize> { with_port(|ws| ws.front_to_back_depths()) }

pub fn window_under_cursor() -> Option<WindowServerId> { with_port(|ws| ws.window_under_cursor()) }

pub fn get_window_at_point(point: CGPoint) -> Option<WindowServerId> {
    with_port(|ws| ws.get_window_at_point(point))
}

pub fn server_frame(id: WindowServerId) -> Option<CGRect> { with_port(|ws| ws.server_frame(id)) }

pub fn corner_radius(id: WindowServerId) -> Option<f64> { with_port(|ws| ws.corner_radius(id)) }

pub fn corner_radii(ids: &[WindowServerId]) -> HashMap<WindowServerId, f64> {
    with_port(|ws| ws.corner_radii(ids))
}

pub fn key_focused_window(space: SpaceId) -> Option<WindowId> {
    with_port(|ws| ws.key_focused_window(space))
}

pub fn key_focus_pid() -> Option<pid_t> { with_port(|ws| ws.key_focus_pid()) }

pub fn on_screen_windows(titles: bool) -> Vec<Listed> {
    with_port(|ws| ws.on_screen_windows(titles))
}

pub fn every_window() -> Vec<Listed> { with_port(|ws| ws.every_window()) }

pub fn stack_level(id: WindowServerId) -> (Option<WindowLevel>, Option<c_int>) {
    with_port(|ws| ws.stack_level(id))
}

pub fn stack_place(id: WindowServerId, frame: CGRect) -> StackPlace {
    with_port(|ws| ws.stack_place(id, frame))
}

/// `facts` with the window server's answers about `id` in, `known` the catalogue's record of a
/// window; none asked for a window with no window-server id.
pub fn with_server_facts(
    facts: Facts,
    id: Option<WindowServerId>,
    known: impl FnOnce(WindowServerId) -> Option<WindowServerInfo>,
) -> Facts {
    match id {
        Some(id) => with_port(|ws| ws.with_server_facts(facts, id, known(id).as_ref())),
        None => facts,
    }
}

pub fn make_key_window(pid: pid_t, id: WindowServerId) -> Result<(), CGError> {
    with_port(|ws| ws.make_key_window(pid, id))
}

pub fn take_stack_place(own: WindowServerId, level: Option<WindowLevel>, sub_level: Option<c_int>) {
    with_port(|ws| ws.take_stack_place(own, level, sub_level));
}

#[cfg(test)]
fn stating(state: impl FnOnce(&FakeWindowServer)) { FAKE.with(state) }

#[cfg(test)]
pub fn set_space_window_list_for_connection_override(ids: Option<Vec<u32>>) {
    stating(|fake| {
        fake.set_any_space_window_list(ids);
    });
}

#[cfg(test)]
pub fn set_space_window_list_for_space_override(space: u64, ids: Option<Vec<u32>>) {
    stating(|fake| {
        fake.set_space_window_list(space, ids);
    });
}

#[cfg(test)]
pub fn set_window_spaces_override(id: WindowServerId, spaces: Option<Vec<u64>>) {
    let spaces = spaces.unwrap_or_default().into_iter().map(SpaceId::new).collect();
    stating(|fake| {
        fake.set_window_spaces(id, spaces);
    });
}

#[cfg(test)]
pub fn set_window_ordered_in_override(id: WindowServerId, ordered: Option<bool>) {
    stating(|fake| {
        fake.set_window_ordered_in(id, ordered);
    });
}

/// The on-screen order, frontmost first, each window at the normal level. `None` clears it.
#[cfg(test)]
pub fn set_front_to_back_override(order: Option<Vec<u32>>) {
    let listed = order
        .unwrap_or_default()
        .into_iter()
        .map(|id| Listed {
            id: WindowServerId::new(id),
            level: 0,
            pid: 1234,
            bounds: CGRect::default(),
            title: None,
        })
        .collect();
    stating(|fake| {
        fake.set_on_screen_windows(listed);
    });
}

/// The window under the cursor, which is the window at every point.
#[cfg(test)]
pub fn set_window_under_cursor_override(window: Option<u32>) {
    stating(|fake| {
        fake.set_window_at_point(window.map(WindowServerId::new));
    });
}

/// `id`'s frame; `None` puts it back at the origin.
#[cfg(test)]
pub fn set_window_frame_override(id: WindowServerId, frame: Option<CGRect>) {
    let info = frame.map(|frame| WindowServerInfo { frame, ..at_origin(id) });
    stating(|fake| {
        fake.set_window(id, info);
    });
}

/// The corners the window server reports for `id`; `None` reports none.
#[cfg(test)]
pub fn set_corner_radii_override(id: WindowServerId, corners: Option<Vec<f64>>) {
    stating(|fake| {
        fake.set_corner_radii(id, corners);
    });
}

#[cfg(test)]
mod tests {
    use super::super::Command;
    use super::*;
    use crate::windows::domain::manageable::ServerFacts;

    /// A window-server id in a test can name a real window on the machine running it, so no read
    /// through the shim reaches the live window server and no command drives it.
    #[test]
    fn a_test_never_reads_or_drives_the_live_window_server() {
        let (id, space) = (WindowServerId::new(10001), SpaceId::new(1));
        assert_eq!(app_window_suitability(id), None);
        assert_eq!(window_ordered_in(id), None);
        assert!(window_spaces(id).is_empty());
        assert_eq!(window_space(id), None);
        assert!(space_window_list_for_connection(&[space.get()]).is_empty());
        assert_eq!(window_parent(id), None);
        assert_eq!(server_frame(id), None);
        assert_eq!(key_focused_window(space), None);
        assert_eq!(key_focus_pid(), None);
        assert_eq!(get_window_at_point(CGPoint::new(1.0, 1.0)), None);
        assert_eq!(stack_level(id), (None, None));
        let asked = Facts {
            minimized: false,
            ax_standard: true,
            ax_root: true,
            server: ServerFacts::default(),
        };
        assert_eq!(with_server_facts(asked, Some(id), |_| None), asked);
        assert_eq!(make_key_window(std::process::id() as pid_t, id), Ok(()));
        take_stack_place(id, Some(3), Some(1));
        assert_eq!(FAKE.with(FakeWindowServer::commands), [
            Command::MakeKeyWindow {
                pid: std::process::id() as pid_t,
                window: id
            },
            Command::TakeStackPlace {
                own: id,
                level: Some(3),
                sub_level: Some(1)
            },
        ]);
    }

    #[test]
    fn an_override_is_an_answer_of_the_shim_s_fake() {
        let id = WindowServerId::new(10002);
        set_window_ordered_in_override(id, Some(false));
        assert_eq!(window_ordered_in(id), Some(false));
        set_window_ordered_in_override(id, None);
        assert_eq!(window_ordered_in(id), None);
        set_corner_radii_override(id, Some(vec![12.0; 4]));
        assert_eq!(corner_radius(id), Some(12.0));
    }

    #[test]
    fn a_window_no_test_stated_is_at_the_origin_until_its_frame_is_stated() {
        let id = WindowServerId::new(10003);
        assert_eq!(
            get_window(id).map(|w| (w.frame, w.pid)),
            Some((at_origin(id).frame, 1234))
        );
        let frame = CGRect::new(CGPoint::new(5.0, 6.0), CGSize::new(7.0, 8.0));
        set_window_frame_override(id, Some(frame));
        assert_eq!(get_windows(&[id]).first().map(|w| w.frame), Some(frame));
        set_window_frame_override(id, None);
        assert_eq!(get_window(id).map(|w| w.frame), Some(at_origin(id).frame));
    }
}
