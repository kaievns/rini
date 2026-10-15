//! Taking a window on: admission, its id, its record, and the AXWindows refresh that takes on what
//! the thread has not seen and rebinds what it has to the element Accessibility hands back now.

use std::num::NonZeroU32;

use rini_core::ids::{WindowId, WindowServerId};
use rustc_hash::FxHashMap as HashMap;
use tracing::{debug, info, trace};

use super::{AppWindowState, State};
use crate::windows::domain::admissible;
use crate::windows::domain::info::{WindowInfo, WindowServerInfo};
use crate::windows::event::Event;
use crate::windows::platform::ax::element::Error as AxError;
use crate::windows::platform::ax::world::AxWorld;

impl<W: AxWorld> State<W> {
    pub(super) fn refresh_visible_windows(&mut self) -> Result<(), AxError> {
        let window_elems = match self.ax.windows(&self.ax.app()) {
            Ok(elems) => elems,
            Err(e) => {
                self.send_event(Event::WindowsDiscovered {
                    pid: self.pid,
                    new: Default::default(),
                    known_visible: Default::default(),
                });
                return Err(e);
            }
        };
        let server_info_by_id = self.visible_window_server_info_map(&window_elems);
        let mut new = Vec::with_capacity(window_elems.len());
        let mut known_visible = Vec::with_capacity(window_elems.len());

        for elem in window_elems {
            let wsid = self.ax.window_server_id(&elem);
            let hint = wsid.and_then(|id| server_info_by_id.get(&id).copied());
            let mut info = match self.ax.window_info(&elem, hint) {
                Ok((info, _)) => info,
                Err(err) => {
                    let id = self.id(&elem).ok();
                    trace!(?id, ?err, "Failed to refresh window info; will retry later");
                    continue;
                }
            };
            if !Self::has_visible_cg_peer(wsid, hint) && !info.is_minimized {
                trace!(pid = ?self.pid, ?wsid, "Ignoring AX window without a visible CG window");
                continue;
            }

            let wid = if let Ok(wid) = self.id(&elem) {
                self.complete_info(&elem, &mut info);
                // The WindowServer id is stable across sleep/display transitions, but
                // the corresponding AXUIElement is not. `id` intentionally resolves the
                // fresh element to the existing wid by that stable id; refresh the actor's
                // handle as well or subsequent frame writes keep targeting the pre-wake
                // element and can never recover.
                self.rebind_window_element(wid, elem, &info);
                wid
            } else {
                let Some((registered, wid, _)) = self.register_window(elem, hint) else {
                    continue;
                };
                info = registered;
                wid
            };

            if !info.is_minimized {
                known_visible.push(wid);
            }
            new.push((wid, info));
        }

        self.send_event(Event::WindowsDiscovered {
            pid: self.pid,
            new,
            known_visible,
        });
        self.on_main_window_changed(None, true);
        Ok(())
    }

    #[must_use]
    pub(super) fn register_window(
        &mut self,
        elem: W::Element,
        server_info_hint: Option<WindowServerInfo>,
    ) -> Option<(WindowInfo, WindowId, Option<WindowServerInfo>)> {
        let Ok((mut info, server_info)) = self.ax.window_info(&elem, server_info_hint) else {
            return None;
        };
        let candidate = admissible::Candidate {
            has_visible_peer: Self::has_visible_cg_peer(info.sys_id, server_info),
            is_minimized: info.is_minimized,
            bundle_id: info.bundle_id.as_deref(),
            path: info.path.as_ref().and_then(|p| p.to_str()),
            ax_role: info.ax_role.as_deref(),
        };
        if let Some(reason) = admissible::rejection(&candidate) {
            trace!(
                pid = ?self.pid,
                sys_id = ?info.sys_id,
                bundle_id = ?info.bundle_id,
                role = ?info.ax_role,
                ?reason,
                "Not registering this AX window"
            );
            return None;
        }

        self.complete_info(&elem, &mut info);

        let window_server_id = info.sys_id.filter(|sid| sid.as_nonzero().is_some()).or_else(|| {
            let id = self.ax.window_server_id(&elem);
            if id.is_none() {
                info!(pid = ?self.pid, "Could not get window server id for a new AX window");
            }
            id
        });

        let idx = window_server_id.and_then(WindowServerId::as_nonzero).unwrap_or_else(|| {
            self.last_window_idx += 1;
            NonZeroU32::new(self.last_window_idx).unwrap()
        });
        let wid = WindowId { pid: self.pid, idx };
        if self.windows.contains_key(&wid) {
            trace!(?wid, "Window already registered; skipping duplicate");
            return None;
        }

        if !self.register_window_notifications(&elem, wid) {
            return None;
        }
        let hidden_by_app = self.is_hidden;
        let last_seen_txid = self.txid_from_store(window_server_id).unwrap_or_default();

        let old = self.windows.insert(wid, AppWindowState {
            elem: elem.clone(),
            last_seen_txid,
            hidden_by_app,
            window_server_id,
            title: info.title.clone(),
        });
        debug_assert!(old.is_none(), "Duplicate window id {wid:?}");
        self.elem_to_wid.insert(elem, wid);
        if hidden_by_app {
            self.send_event(Event::WindowMinimized(wid));
        }
        Some((info, wid, server_info))
    }

    /// What Accessibility's own read leaves out of `info`: whether a window of an application
    /// `admissible::needs_title_element_to_be_standard` names is standard, and whether the window
    /// server gives it a parent.
    fn complete_info(&self, elem: &W::Element, info: &mut WindowInfo) {
        if admissible::needs_title_element_to_be_standard(self.bundle_id.as_deref())
            && self.ax.read_attribute(elem, "AXTitleUIElement").is_err()
        {
            info.is_standard = false;
        }
        info.is_root =
            info.sys_id.is_none_or(|wsid| self.ax.server().window_parent(wsid).is_none());
    }

    fn rebind_window_element(&mut self, wid: WindowId, elem: W::Element, info: &WindowInfo) {
        let Some(old_elem) = self.windows.get(&wid).map(|window| window.elem.clone()) else {
            return;
        };
        if old_elem == elem {
            return;
        }

        // Move observer ownership before replacing the handle. Removing from an
        // invalid old element can legitimately fail; Observer retains its callback
        // context in that case, so a late notification remains memory-safe and its
        // encoded wid still resolves to this logical window.
        self.remove_window_notifications(&old_elem);
        if !self.register_window_notifications(&elem, wid) {
            // Keep the last usable binding and restore its notifications when the
            // replacement cannot yet be observed. A later AXWindows refresh retries.
            self.remove_window_notifications(&elem);
            let _ = self.register_window_notifications(&old_elem, wid);
            return;
        }

        self.elem_to_wid.remove(&old_elem);
        self.elem_to_wid.insert(elem.clone(), wid);
        if let Some(window) = self.windows.get_mut(&wid) {
            window.elem = elem;
            window.window_server_id = info.sys_id.or(window.window_server_id);
            window.title = info.title.clone();
        }
        debug!(?wid, "Rebound window to refreshed AX element");
    }

    pub(super) fn visible_window_server_info_map(
        &self,
        window_elements: &[W::Element],
    ) -> HashMap<WindowServerId, WindowServerInfo> {
        let wsids: Vec<WindowServerId> = window_elements
            .iter()
            .filter_map(|elem| self.ax.window_server_id(elem))
            .collect();
        let mut info_by_id = HashMap::with_capacity_and_hasher(wsids.len(), Default::default());
        for info in self.ax.server().get_windows(&wsids) {
            info_by_id.insert(info.id, info);
        }
        info_by_id
    }

    #[inline]
    pub(super) fn has_visible_cg_peer(
        wsid: Option<WindowServerId>,
        hint: Option<WindowServerInfo>,
    ) -> bool {
        admissible::has_visible_peer(wsid.is_some(), hint.is_some())
    }
}

#[cfg(test)]
mod tests {
    use super::super::fixtures::{
        Sink, announced, app_with, minimized_and_restored, peer, rect, state_with,
    };
    use super::*;
    use crate::windows::domain::ax_events::AxNotificationKind;
    use crate::windows::domain::request::Request;
    use crate::windows::platform::ax::world::FakeAx;

    /// The rule `admissible::needs_title_element_to_be_standard` names, end to end: for the
    /// applications it lists, a window whose `AXTitleUIElement` cannot be read is not standard.
    #[test]
    fn a_window_without_a_readable_title_element_is_not_standard() {
        let window = 7;
        let mut ax = FakeAx::with_one_window(window, rect(0., 0., 100., 100.));
        ax.without_title_element.push(window);
        let (mut state, _sink) = state_with(ax);
        state.bundle_id = Some("com.googlecode.iterm2".to_owned());

        let (info, _, _) = state
            .register_window(window, peer(window, rect(0., 0., 100., 100.)))
            .expect("registers");

        assert!(!info.is_standard, "no readable title element means not standard");
    }

    /// A window the window server gives a parent is a child window, not a root one.
    #[test]
    fn a_window_the_window_server_gives_a_parent_is_not_a_root_window() {
        let window = 7;
        let frame = rect(0., 0., 100., 100.);
        let ax = FakeAx::with_one_window(window, frame);
        ax.server
            .set_window_parent(WindowServerId::new(window), Some(WindowServerId::new(3)));
        let (mut state, _sink) = state_with(ax);

        let (info, _, _) = state.register_window(window, peer(window, frame)).expect("registers");

        assert!(!info.is_root);
    }

    #[test]
    fn a_window_taken_on_while_the_application_is_hidden_is_announced_minimized() {
        let (mut state, sink, _) = app_with(&[7]);
        state.handle_notification(1, AxNotificationKind::ApplicationHidden, None);
        state.ax.describe_standard_window(8, rect(0., 0., 100., 100.));
        announced(&sink);

        let (_, wid, _) =
            state.register_window(8, peer(8, rect(0., 0., 100., 100.))).expect("registers");

        assert_eq!(minimized_and_restored(&announced(&sink)), (vec![wid], vec![]));
    }

    /// A refresh takes on the windows the window server reports, and leaves out one it does not,
    /// which is not on screen.
    #[test]
    fn a_refresh_takes_on_only_the_windows_the_window_server_reports() {
        let (reported, unreported) = (7, 8);
        let frame = rect(0., 0., 100., 100.);
        let mut ax = FakeAx::with_one_window(reported, frame);
        ax.windows.push(unreported);
        ax.describe_standard_window(unreported, frame);
        ax.server.set_window(WindowServerId::new(reported), peer(reported, frame));
        let (mut state, sink) = state_with(ax);

        state.handle_request(Request::GetVisibleWindows).expect("refreshed");

        let reported_wid = state.id(&reported).expect("taken on");
        assert!(state.id(&unreported).is_err(), "left out");
        assert!(
            sink.0.borrow().iter().any(|event| matches!(
                event,
                Event::WindowsDiscovered { new, .. }
                    if new.iter().map(|(w, _)| *w).eq([reported_wid])
            )),
            "{:?}",
            sink.0.borrow()
        );
    }

    /// What the refresh `GetVisibleWindows` asks for reports: each window it found, with its info,
    /// and the ones of those not minimized.
    fn refreshed(
        state: &mut State<FakeAx>,
        sink: &Sink,
    ) -> (Vec<(WindowId, WindowInfo)>, Vec<WindowId>) {
        sink.0.borrow_mut().clear();
        state.handle_request(Request::GetVisibleWindows).expect("refreshed");
        sink.0
            .borrow()
            .iter()
            .find_map(|event| match event {
                Event::WindowsDiscovered { new, known_visible, .. } => {
                    Some((new.clone(), known_visible.clone()))
                }
                _ => None,
            })
            .expect("a refresh reports what it found")
    }

    /// A window a refresh takes on is reported once, not once as registered and again as found.
    #[test]
    fn a_refresh_reports_a_window_it_takes_on_once() {
        let window = 7;
        let frame = rect(0., 0., 100., 100.);
        let ax = FakeAx::with_one_window(window, frame);
        ax.server.set_window(WindowServerId::new(window), peer(window, frame));
        let (mut state, sink) = state_with(ax);

        let (new, known_visible) = refreshed(&mut state, &sink);

        let wid = state.id(&window).expect("taken on");
        assert_eq!(new.iter().map(|(w, _)| *w).collect::<Vec<_>>(), [wid]);
        assert_eq!(known_visible, [wid]);
    }

    /// What registration decides about a window holds at every refresh after: a window of an
    /// application that needs a title element and has none stays non-standard, and one the window
    /// server gives a parent stays off the root.
    #[test]
    fn a_refresh_reports_a_known_window_as_registration_decided() {
        let (untitled, child) = (7, 8);
        let frame = rect(0., 0., 100., 100.);
        let mut ax = FakeAx::with_one_window(untitled, frame);
        ax.windows.push(child);
        ax.describe_standard_window(child, frame);
        ax.without_title_element.push(untitled);
        for window in [untitled, child] {
            ax.server.set_window(WindowServerId::new(window), peer(window, frame));
        }
        ax.server
            .set_window_parent(WindowServerId::new(child), Some(WindowServerId::new(3)));
        let (mut state, sink) = state_with(ax);
        state.bundle_id = Some("com.googlecode.iterm2".to_owned());
        refreshed(&mut state, &sink);

        let (new, _) = refreshed(&mut state, &sink);

        let info = |window| {
            let wid = state.id(&window).expect("known");
            new.iter()
                .find(|(w, _)| *w == wid)
                .map(|(_, info)| info.clone())
                .expect("reported")
        };
        assert!(!info(untitled).is_standard, "no title element, not standard");
        assert!(!info(child).is_root, "a parent, not a root window");
        assert_eq!(new.len(), 2, "each reported once: {new:?}");
    }
}
