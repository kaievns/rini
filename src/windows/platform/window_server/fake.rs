//! A window server a test states the answers of.

use std::collections::HashMap;
use std::ffi::c_int;
use std::hash::Hash;
use std::sync::{Mutex, MutexGuard, PoisonError};

use objc2_core_foundation::{CGPoint, CGRect};
use objc2_core_graphics::CGError;
use rini_core::ids::{SpaceId, WindowId, WindowServerId, pid_t};

use super::{Command, Listed, WindowServer, on_more_than_one_space};
use crate::windows::domain::info::{WindowServerInfo, reported_radius};
use crate::windows::domain::stack::WindowLevel;

/// Every read answers "unknown", `None` or an empty list, until a test states its answer with the
/// `set_` method named for it; stating `None` makes it unknown again. Commands are recorded, never
/// carried out, and `make_key_window` succeeds unless `fail_make_key_window` says otherwise.
///
/// A read the live window server answers from another read's answer is answered here from the
/// same stated fact, so no test states a pair production cannot produce: stickiness from the
/// spaces, the level from the window's layer, the depths from the on-screen list, the window under
/// the cursor from the window at a point.
///
/// Answers are behind a lock, so a test keeps one `Arc` and hands clones of it, as
/// `Arc<dyn WindowServer>`, to the threads under test, and states answers on its own while they run.
#[derive(Debug, Default)]
pub struct FakeWindowServer {
    answers: Mutex<Answers>,
    commands: Mutex<Vec<Command>>,
}

#[derive(Debug, Default)]
struct Answers {
    windows: HashMap<WindowServerId, WindowServerInfo>,
    parents: HashMap<WindowServerId, WindowServerId>,
    sub_levels: HashMap<WindowServerId, c_int>,
    spaces: HashMap<WindowServerId, Vec<SpaceId>>,
    user_spaces: HashMap<SpaceId, bool>,
    ordered_in: HashMap<WindowServerId, bool>,
    suitable: HashMap<WindowServerId, bool>,
    space_lists: HashMap<u64, Vec<u32>>,
    any_space_list: Option<Vec<u32>>,
    at_point: Option<WindowServerId>,
    server_frames: HashMap<WindowServerId, CGRect>,
    corners: HashMap<WindowServerId, Vec<f64>>,
    key_focused: HashMap<SpaceId, WindowId>,
    key_focus_pid: Option<pid_t>,
    on_screen: Vec<Listed>,
    every: Vec<Listed>,
    make_key_error: Option<CGError>,
}

fn locked<T>(lock: &Mutex<T>) -> MutexGuard<'_, T> {
    lock.lock().unwrap_or_else(PoisonError::into_inner)
}

fn state<K: Eq + Hash, V>(map: &mut HashMap<K, V>, key: K, answer: Option<V>) {
    match answer {
        Some(answer) => map.insert(key, answer),
        None => map.remove(&key),
    };
}

impl FakeWindowServer {
    fn answer<R>(&self, read: impl FnOnce(&Answers) -> R) -> R { read(&locked(&self.answers)) }

    fn stating(&self, write: impl FnOnce(&mut Answers)) -> &Self {
        write(&mut locked(&self.answers));
        self
    }

    /// The commands given so far, in order.
    pub fn commands(&self) -> Vec<Command> { locked(&self.commands).clone() }

    /// `id` as `get_window` answers it, its `layer` the level `window_level` answers.
    pub fn set_window(&self, id: WindowServerId, info: Option<WindowServerInfo>) -> &Self {
        self.stating(|a| state(&mut a.windows, id, info))
    }

    pub fn set_window_parent(&self, id: WindowServerId, parent: Option<WindowServerId>) -> &Self {
        self.stating(|a| state(&mut a.parents, id, parent))
    }

    pub fn set_window_sub_level(&self, id: WindowServerId, sub_level: Option<c_int>) -> &Self {
        self.stating(|a| state(&mut a.sub_levels, id, sub_level))
    }

    /// The spaces `id` is on, which `window_is_sticky` counts; none is unknown.
    pub fn set_window_spaces(&self, id: WindowServerId, spaces: Vec<SpaceId>) -> &Self {
        self.stating(|a| state(&mut a.spaces, id, (!spaces.is_empty()).then_some(spaces)))
    }

    pub fn set_space_is_user(&self, space: SpaceId, user: Option<bool>) -> &Self {
        self.stating(|a| state(&mut a.user_spaces, space, user))
    }

    pub fn set_window_ordered_in(&self, id: WindowServerId, ordered_in: Option<bool>) -> &Self {
        self.stating(|a| state(&mut a.ordered_in, id, ordered_in))
    }

    pub fn set_app_window_suitability(&self, id: WindowServerId, suitable: Option<bool>) -> &Self {
        self.stating(|a| state(&mut a.suitable, id, suitable))
    }

    /// The windows on `space`, answered to a query of that space alone.
    pub fn set_space_window_list(&self, space: u64, ids: Option<Vec<u32>>) -> &Self {
        self.stating(|a| state(&mut a.space_lists, space, ids))
    }

    /// The windows answered to a query of any spaces `set_space_window_list` states none for.
    pub fn set_any_space_window_list(&self, ids: Option<Vec<u32>>) -> &Self {
        self.stating(|a| a.any_space_list = ids)
    }

    /// The window `get_window_at_point` answers, at every point, the cursor's included.
    pub fn set_window_at_point(&self, id: Option<WindowServerId>) -> &Self {
        self.stating(|a| a.at_point = id)
    }

    pub fn set_server_frame(&self, id: WindowServerId, frame: Option<CGRect>) -> &Self {
        self.stating(|a| state(&mut a.server_frames, id, frame))
    }

    /// The four corners the window server reports for `id`, decoded as the live read decodes them.
    pub fn set_corner_radii(&self, id: WindowServerId, corners: Option<Vec<f64>>) -> &Self {
        self.stating(|a| state(&mut a.corners, id, corners))
    }

    pub fn set_key_focused_window(&self, space: SpaceId, window: Option<WindowId>) -> &Self {
        self.stating(|a| state(&mut a.key_focused, space, window))
    }

    pub fn set_key_focus_pid(&self, pid: Option<pid_t>) -> &Self {
        self.stating(|a| a.key_focus_pid = pid)
    }

    /// The windows on screen, frontmost first, which `front_to_back_depths` counts.
    pub fn set_on_screen_windows(&self, listed: Vec<Listed>) -> &Self {
        self.stating(|a| a.on_screen = listed)
    }

    pub fn set_every_window(&self, listed: Vec<Listed>) -> &Self {
        self.stating(|a| a.every = listed)
    }

    /// `make_key_window` records its command and then fails with `error`; `None` lets it succeed.
    pub fn fail_make_key_window(&self, error: Option<CGError>) -> &Self {
        self.stating(|a| a.make_key_error = error)
    }
}

impl WindowServer for FakeWindowServer {
    fn get_window(&self, id: WindowServerId) -> Option<WindowServerInfo> {
        self.answer(|a| a.windows.get(&id).copied())
    }

    fn get_windows(&self, ids: &[WindowServerId]) -> Vec<WindowServerInfo> {
        self.answer(|a| ids.iter().filter_map(|id| a.windows.get(id).copied()).collect())
    }

    fn window_parent(&self, id: WindowServerId) -> Option<WindowServerId> {
        self.answer(|a| a.parents.get(&id).copied())
    }

    fn window_is_sticky(&self, id: WindowServerId) -> Option<bool> {
        self.answer(|a| Some(on_more_than_one_space(a.spaces.get(&id)?.len())))
    }

    fn window_level(&self, id: WindowServerId) -> Option<WindowLevel> {
        self.answer(|a| Some(a.windows.get(&id)?.layer as WindowLevel))
    }

    fn window_sub_level(&self, id: WindowServerId) -> Option<c_int> {
        self.answer(|a| a.sub_levels.get(&id).copied())
    }

    fn window_spaces(&self, id: WindowServerId) -> Vec<SpaceId> {
        self.answer(|a| a.spaces.get(&id).cloned().unwrap_or_default())
    }

    fn space_is_user(&self, space: SpaceId) -> Option<bool> {
        self.answer(|a| a.user_spaces.get(&space).copied())
    }

    fn window_ordered_in(&self, id: WindowServerId) -> Option<bool> {
        self.answer(|a| a.ordered_in.get(&id).copied())
    }

    fn app_window_suitability(&self, id: WindowServerId) -> Option<bool> {
        self.answer(|a| a.suitable.get(&id).copied())
    }

    fn space_window_list_for_connection(&self, spaces: &[u64]) -> Vec<u32> {
        self.answer(|a| {
            if let [space] = spaces
                && let Some(ids) = a.space_lists.get(space)
            {
                return ids.clone();
            }
            a.any_space_list.clone().unwrap_or_default()
        })
    }

    fn window_under_cursor(&self) -> Option<WindowServerId> { self.answer(|a| a.at_point) }

    fn get_window_at_point(&self, _: CGPoint) -> Option<WindowServerId> {
        self.answer(|a| a.at_point)
    }

    fn server_frame(&self, id: WindowServerId) -> Option<CGRect> {
        self.answer(|a| a.server_frames.get(&id).copied())
    }

    fn corner_radii(&self, ids: &[WindowServerId]) -> HashMap<WindowServerId, f64> {
        self.answer(|a| {
            ids.iter()
                .filter_map(|&id| Some((id, reported_radius(a.corners.get(&id)?)?)))
                .collect()
        })
    }

    fn key_focused_window(&self, space: SpaceId) -> Option<WindowId> {
        self.answer(|a| a.key_focused.get(&space).copied())
    }

    fn key_focus_pid(&self) -> Option<pid_t> { self.answer(|a| a.key_focus_pid) }

    fn on_screen_windows(&self, titles: bool) -> Vec<Listed> {
        self.answer(|a| {
            let mut listed = a.on_screen.clone();
            if !titles {
                listed.iter_mut().for_each(|window| window.title = None);
            }
            listed
        })
    }

    fn every_window(&self) -> Vec<Listed> { self.answer(|a| a.every.clone()) }

    fn make_key_window(&self, pid: pid_t, window: WindowServerId) -> Result<(), CGError> {
        locked(&self.commands).push(Command::MakeKeyWindow { pid, window });
        self.answer(|a| a.make_key_error).map_or(Ok(()), Err)
    }

    fn take_stack_place(
        &self,
        own: WindowServerId,
        level: Option<WindowLevel>,
        sub_level: Option<c_int>,
    ) {
        locked(&self.commands).push(Command::TakeStackPlace { own, level, sub_level });
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use objc2_core_foundation::CGSize;

    use super::*;
    use crate::windows::domain::manageable::ServerFacts;

    fn id(n: u32) -> WindowServerId { WindowServerId::new(n) }

    /// A read with nothing stated must answer "unknown", never a guess: an unanswerable query is no
    /// evidence, while a negative one retires the window. See "A unit test must not read the live
    /// window server" in `docs/testing.md`.
    #[test]
    fn every_read_answers_unknown_with_no_setup() {
        let ws = FakeWindowServer::default();
        let (window, space) = (id(10001), SpaceId::new(1));
        assert_eq!(ws.get_window(window).map(|w| w.id), None);
        assert!(ws.get_windows(&[window]).is_empty());
        assert_eq!(ws.window_parent(window), None);
        assert_eq!(ws.window_is_sticky(window), None);
        assert_eq!(ws.window_level(window), None);
        assert_eq!(ws.window_sub_level(window), None);
        assert!(ws.window_spaces(window).is_empty());
        assert_eq!(ws.space_is_user(space), None);
        assert_eq!(ws.window_space(window), None);
        assert_eq!(ws.window_ordered_in(window), None);
        assert_eq!(ws.app_window_suitability(window), None);
        assert!(ws.space_window_list_for_connection(&[space.get()]).is_empty());
        assert!(ws.front_to_back_depths().is_empty());
        assert_eq!(ws.window_under_cursor(), None);
        assert_eq!(ws.get_window_at_point(CGPoint::new(10.0, 10.0)), None);
        assert_eq!(ws.server_frame(window), None);
        assert!(ws.corner_radii(&[window]).is_empty());
        assert_eq!(ws.corner_radius(window), None);
        assert_eq!(ws.key_focused_window(space), None);
        assert_eq!(ws.key_focus_pid(), None);
        assert!(ws.on_screen_windows(true).is_empty());
        assert!(ws.every_window().is_empty());
        assert_eq!(ws.stack_level(window), (None, None));
        assert_eq!(ws.server_facts(window, None), ServerFacts::default());
        assert!(ws.commands().is_empty(), "a read gives no command");
    }

    #[test]
    fn make_key_window_and_take_stack_place_only_record() {
        let ws = FakeWindowServer::default();
        assert_eq!(ws.make_key_window(7, id(30)), Ok(()));
        ws.take_stack_place(id(2), Some(3), None);
        assert_eq!(ws.commands(), [
            Command::MakeKeyWindow { pid: 7, window: id(30) },
            Command::TakeStackPlace {
                own: id(2),
                level: Some(3),
                sub_level: None
            },
        ]);
        ws.fail_make_key_window(Some(CGError::Failure));
        assert_eq!(ws.make_key_window(7, id(31)), Err(CGError::Failure));
        assert_eq!(ws.commands().len(), 3, "a failing command is recorded too");
    }

    #[test]
    fn a_stated_answer_is_answered_until_it_is_stated_unknown_again() {
        let ws = FakeWindowServer::default();
        ws.set_window_ordered_in(id(2), Some(false))
            .set_window_parent(id(2), Some(id(1)));
        assert_eq!(
            (ws.window_ordered_in(id(2)), ws.window_parent(id(2))),
            (Some(false), Some(id(1)))
        );
        assert_eq!(
            ws.window_ordered_in(id(3)),
            None,
            "only for the window it was stated for"
        );
        ws.set_window_ordered_in(id(2), None);
        assert_eq!(ws.window_ordered_in(id(2)), None);
    }

    #[test]
    fn one_windows_corner_radius_is_the_one_its_query_reports() {
        let ws = FakeWindowServer::default();
        let (reported, silent) = (id(10041), id(10042));
        ws.set_corner_radii(reported, Some(vec![12.0; 4]));
        assert_eq!(
            (ws.corner_radius(reported), ws.corner_radius(silent)),
            (Some(12.0), None)
        );
    }

    #[test]
    fn a_window_s_home_is_the_user_space_among_the_spaces_it_is_on() {
        let ws = FakeWindowServer::default();
        let (fullscreen, desktop) = (SpaceId::new(9), SpaceId::new(4));
        ws.set_window_spaces(id(5), vec![fullscreen, desktop]);
        assert_eq!(
            ws.window_space(id(5)),
            Some(fullscreen),
            "with no space known for a user one"
        );
        ws.set_space_is_user(fullscreen, Some(false))
            .set_space_is_user(desktop, Some(true));
        assert_eq!(ws.window_space(id(5)), Some(desktop));
    }

    #[test]
    fn a_space_list_stated_for_one_space_answers_that_space_alone() {
        let ws = FakeWindowServer::default();
        ws.set_space_window_list(1, Some(vec![30, 31]))
            .set_any_space_window_list(Some(vec![40]));
        assert_eq!(ws.space_window_list_for_connection(&[1]), [30, 31]);
        assert_eq!(ws.space_window_list_for_connection(&[2]), [40]);
        assert_eq!(ws.space_window_list_for_connection(&[1, 2]), [40]);
    }

    fn window(n: u32, layer: i32) -> WindowServerInfo {
        WindowServerInfo {
            id: id(n),
            pid: 1,
            layer,
            frame: CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(10.0, 10.0)),
            min_frame: CGSize::ZERO,
            max_frame: CGSize::ZERO,
        }
    }

    /// The live window server answers both from one `SLSCopySpacesForWindows`.
    #[test]
    fn a_window_is_sticky_exactly_when_it_is_stated_on_more_than_one_space() {
        let ws = FakeWindowServer::default();
        let (a, b) = (SpaceId::new(1), SpaceId::new(2));
        ws.set_window_spaces(id(5), vec![a]).set_window_spaces(id(6), vec![a, b]);
        assert_eq!(
            (ws.window_is_sticky(id(5)), ws.window_is_sticky(id(6))),
            (Some(false), Some(true))
        );
        assert_eq!(ws.window_is_sticky(id(7)), None, "no spaces stated is unknown");
    }

    /// The live window server answers both from one `SLSWindowIteratorGetLevel`.
    #[test]
    fn a_window_s_level_is_the_layer_it_was_stated_with() {
        let ws = FakeWindowServer::default();
        ws.set_window(id(5), Some(window(5, 3))).set_window_sub_level(id(5), Some(1));
        let frame = CGRect::new(CGPoint::new(1.0, 2.0), CGSize::new(3.0, 4.0));
        let place = ws.stack_place(id(5), frame);
        assert_eq!(
            (place.frame, place.level, place.sub_level),
            (frame, Some(3), Some(1))
        );
        ws.set_window(id(5), None);
        assert_eq!(ws.window_level(id(5)), None);
    }

    /// The live window server counts the depths over its on-screen list, the desktop left out.
    #[test]
    fn the_depths_are_the_stated_on_screen_list_without_the_desktop() {
        let ws = FakeWindowServer::default();
        let listed = |n: u32, level: i32| Listed {
            id: id(n),
            level,
            pid: 1,
            bounds: CGRect::default(),
            title: None,
        };
        ws.set_on_screen_windows(vec![listed(30, 0), listed(2, -2147483626), listed(31, 3)]);
        assert_eq!(ws.front_to_back_depths(), HashMap::from([(30, 0), (31, 1)]));
    }

    /// The live window server answers the window at the cursor's point.
    #[test]
    fn the_window_under_the_cursor_is_the_window_at_a_point() {
        let ws = FakeWindowServer::default();
        ws.set_window_at_point(Some(id(9)));
        assert_eq!(ws.window_under_cursor(), Some(id(9)));
        assert_eq!(ws.window_under_cursor(), ws.get_window_at_point(CGPoint::ZERO));
    }

    #[test]
    fn a_window_s_manageability_facts_are_its_stated_answers() {
        let ws = FakeWindowServer::default();
        ws.set_window(id(5), Some(window(5, 0)))
            .set_window_spaces(id(5), vec![SpaceId::new(1), SpaceId::new(2)]);
        assert_eq!(
            ws.server_facts(id(5), ws.get_window(id(5)).as_ref()),
            ServerFacts {
                layer: Some(0),
                sticky: Some(true),
                level: Some(0),
            }
        );
    }

    #[test]
    fn a_clone_of_the_port_hears_answers_stated_after_it_was_handed_out() {
        let fake = Arc::new(FakeWindowServer::default());
        let port: Arc<dyn WindowServer> = fake.clone();
        let asked = std::thread::spawn({
            let port = port.clone();
            move || port.window_ordered_in(id(2))
        });
        assert_eq!(asked.join().unwrap(), None);
        fake.set_window_ordered_in(id(2), Some(true));
        assert_eq!(
            std::thread::spawn(move || port.window_ordered_in(id(2))).join().unwrap(),
            Some(true)
        );
        assert!(fake.commands().is_empty());
    }
}
