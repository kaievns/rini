//! What the flight engine asks of the pictures service: the windows on screen at a moment of a
//! flight, each under the engine's tag and where it is expected, the pictures warmed and refreshed,
//! and the end state's watches, one slot a kind retargeted from one generation to the next.

use rini_core::ids::{WindowId, WindowServerId};
use rini_geometry::space::ScreenRect;

use crate::pictures::domain::target::PictureTarget;
use crate::pictures::domain::watch::{
    CHANGE_EVERY, CHANGE_TRIES, Client, END_SLACK, LIVE_EVERY, Moment, OnScreen, Settle, Tag,
    Watch, WatchId,
};
use crate::pictures::platform::service::Pictures;
use crate::transitions::domain::lifecycle::EndWatch;

/// What the pictures service is asked for `window` drawn at `frame`.
fn target_at(window: WindowId, frame: ScreenRect) -> PictureTarget {
    PictureTarget {
        window,
        server_id: WindowServerId::from(window),
        size: frame.size,
    }
}

fn targets_at(windows: &[(WindowId, ScreenRect)]) -> Vec<(PictureTarget, ScreenRect)> {
    windows
        .iter()
        .map(|&(window, frame)| (target_at(window, frame), frame))
        .collect()
}

fn server_ids(windows: &[WindowId]) -> Vec<WindowServerId> {
    windows.iter().map(|window| WindowServerId::from(*window)).collect()
}

/// The windows on screen at `strip` and `background`, asked for under `tag` at `moment`.
pub(super) fn on_screen(
    tag: Tag,
    moment: Moment,
    strip: &[(WindowId, ScreenRect)],
    background: &[(WindowId, ScreenRect)],
    behind: &[WindowId],
) -> OnScreen {
    OnScreen {
        tag,
        moment,
        strip: targets_at(strip),
        background: targets_at(background),
        behind: server_ids(behind),
    }
}

/// Each window at the size of `frame`: what the quiet warms, at the frame each ended at, and what a
/// turned look is asked for at.
pub(super) fn sized_targets(windows: Vec<(WindowId, ScreenRect)>) -> Vec<PictureTarget> {
    windows.into_iter().map(|(window, frame)| target_at(window, frame)).collect()
}

/// The end state's watches: one slot for each kind, kept from one generation to the next and
/// retargeted, so a window already landed stays landed and a change watch stays armed.
#[derive(Debug, Default, PartialEq)]
pub(super) struct EndWatches {
    landing: Option<WatchId>,
    awaiting: Option<WatchId>,
    change: Option<WatchId>,
    live: Option<WatchId>,
}

impl EndWatches {
    /// Watches what `end` names, in place of what was watched, under `client`'s tag.
    pub(super) fn watch(&mut self, pictures: &Pictures, client: Client, end: &EndWatch) {
        let tag = Tag {
            client,
            generation: end.generation.0,
        };
        let behind = server_ids(&end.behind);
        let landing = |windows: &[(WindowId, ScreenRect)], settle, until| {
            (!windows.is_empty()).then(|| Watch::Landing {
                tag,
                windows: targets_at(windows),
                behind: behind.clone(),
                slack: END_SLACK,
                settle,
                until,
            })
        };
        let watches = [
            landing(&end.landing, Settle::AsSoonAsThere, end.landing_until),
            landing(&end.awaiting, Settle::Repainted, end.awaiting_until),
            end.focus.map(|(window, frame)| Watch::Change {
                tag,
                window: target_at(window, frame),
                end: frame,
                losing: end.losing_focus.map(|(window, frame)| (target_at(window, frame), frame)),
                behind: behind.clone(),
                every: CHANGE_EVERY,
                tries: CHANGE_TRIES,
            }),
        ];
        for (slot, watch) in [&mut self.landing, &mut self.awaiting, &mut self.change]
            .into_iter()
            .zip(watches)
        {
            put(slot, watch, pictures);
        }
        let live = (!end.background.is_empty()).then(|| Watch::Live {
            tag,
            windows: end
                .background
                .iter()
                .map(|&(window, frame)| target_at(window, frame))
                .collect(),
            every: LIVE_EVERY,
            after: self.change,
            initial: end.background_initial,
        });
        put(&mut self.live, live, pictures);
    }

    pub(super) fn stop(&mut self, pictures: &Pictures) {
        for slot in [
            &mut self.landing,
            &mut self.awaiting,
            &mut self.change,
            &mut self.live,
        ] {
            put(slot, None, pictures);
        }
    }
}

/// `watch` in `slot`: retargeting the watch there, starting one, or stopping it.
fn put(slot: &mut Option<WatchId>, watch: Option<Watch>, pictures: &Pictures) {
    match (*slot, watch) {
        (Some(id), Some(watch)) => pictures.retarget(id, watch),
        (None, Some(watch)) => *slot = Some(pictures.watch(watch)),
        (Some(id), None) => {
            pictures.stop(id);
            *slot = None;
        }
        (None, None) => {}
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use rini_runloop::channel;

    use super::*;
    use crate::animation::domain::motion::testing::rect;
    use crate::pictures::platform::service::Command as Asked;
    use crate::transitions::domain::lifecycle::Generation;
    use crate::transitions::domain::timing::HOLD_CAP;

    fn wid(idx: u32) -> WindowId { WindowId::new(7, idx) }

    fn asked(commands: &mut channel::Receiver<Asked>) -> Vec<Asked> {
        let mut out = Vec::new();
        while let Ok((_, command)) = commands.try_recv() {
            out.push(command);
        }
        out
    }

    fn end(generation: u64, now: Instant) -> EndWatch {
        EndWatch {
            generation: Generation(generation),
            compared: vec![
                (wid(1), rect(4.0, 32.0, 859.0, 1081.0)),
                (wid(2), rect(867.0, 32.0, 859.0, 1081.0)),
            ],
            landing: vec![(wid(1), rect(4.0, 32.0, 859.0, 1081.0))],
            awaiting: vec![(wid(2), rect(867.0, 32.0, 859.0, 1081.0))],
            landing_until: now + Duration::from_millis(150),
            awaiting_until: now + HOLD_CAP,
            focus: Some((wid(1), rect(4.0, 32.0, 859.0, 1081.0))),
            losing_focus: Some((wid(3), rect(867.0, 32.0, 859.0, 1081.0))),
            background: vec![(wid(9), rect(224.0, 95.0, 600.0, 400.0))],
            background_initial: Some(now + Duration::from_millis(30)),
            behind: vec![wid(9)],
        }
    }

    #[test]
    fn the_end_state_is_watched_one_slot_a_kind_and_retargeted_from_one_generation_to_the_next() {
        let (pictures, mut commands) = Pictures::detached();
        let client = Client(1);
        let now = Instant::now();
        let mut watches = EndWatches::default();
        watches.watch(&pictures, client, &end(3, now));
        let tag = Tag { client, generation: 3 };
        let first = asked(&mut commands);
        assert_eq!(first.len(), 4, "{first:?}");
        match &first[0] {
            Asked::Watch(
                id,
                Watch::Landing {
                    tag: under,
                    windows,
                    behind,
                    slack,
                    settle,
                    until,
                },
            ) => {
                assert_eq!(Some(*id), watches.landing);
                assert_eq!(*under, tag);
                let target = &windows[0].0;
                assert_eq!(
                    (target.window, target.server_id),
                    (wid(1), WindowServerId::new(1))
                );
                assert_eq!(
                    *behind,
                    vec![WindowServerId::new(9)],
                    "the strip is captured over them"
                );
                assert_eq!(
                    (*slack, *settle, *until),
                    (
                        END_SLACK,
                        Settle::AsSoonAsThere,
                        now + Duration::from_millis(150)
                    )
                );
            }
            other => panic!("{other:?}"),
        }
        match &first[1] {
            Asked::Watch(id, Watch::Landing { windows, settle, until, .. }) => {
                assert_eq!(Some(*id), watches.awaiting);
                assert_eq!(windows[0].0.window, wid(2));
                assert_eq!(
                    (*settle, *until),
                    (Settle::Repainted, now + HOLD_CAP),
                    "watched as long as it is held for"
                );
            }
            other => panic!("{other:?}"),
        }
        match &first[2] {
            Asked::Watch(
                id,
                Watch::Change {
                    window, losing, every, tries, ..
                },
            ) => {
                assert_eq!(Some(*id), watches.change);
                assert_eq!(window.window, wid(1));
                let (losing, at) = losing.expect("the window focus leaves, beside it");
                assert_eq!(
                    (losing.window, losing.size, at),
                    (wid(3), at.size, rect(867.0, 32.0, 859.0, 1081.0))
                );
                assert_eq!((*every, *tries), (CHANGE_EVERY, CHANGE_TRIES));
            }
            other => panic!("{other:?}"),
        }
        match &first[3] {
            Asked::Watch(
                _,
                Watch::Live {
                    windows, every, after, initial, ..
                },
            ) => {
                assert_eq!(windows[0].window, wid(9));
                assert_eq!(*every, LIVE_EVERY);
                assert_eq!(
                    *after, watches.change,
                    "kept fresh once the focus target's look is in"
                );
                assert_eq!(*initial, Some(now + Duration::from_millis(30)));
            }
            other => panic!("{other:?}"),
        }

        // The next generation: everything has landed but what repaints, and the focus is in.
        let mut next = end(4, now);
        next.landing.clear();
        next.focus = None;
        next.losing_focus = None;
        let (awaiting, live) = (watches.awaiting, watches.live);
        watches.watch(&pictures, client, &next);
        let second = asked(&mut commands);
        assert!(
            matches!(&second[..], [
                Asked::Stop(_),
                Asked::Retarget(_, Watch::Landing { settle: Settle::Repainted, .. }),
                Asked::Stop(_),
                Asked::Retarget(_, Watch::Live { after: None, .. }),
            ]),
            "{second:?}"
        );
        assert_eq!(
            (watches.awaiting, watches.live),
            (awaiting, live),
            "retargeted, not watched afresh"
        );
        assert_eq!((watches.landing, watches.change), (None, None));

        watches.stop(&pictures);
        assert!(matches!(&asked(&mut commands)[..], [
            Asked::Stop(_),
            Asked::Stop(_)
        ]));
        assert_eq!(watches, EndWatches::default());
    }

    #[test]
    fn an_ask_of_windows_on_screen_carries_the_engines_tag_and_each_window_where_it_is_expected() {
        let client = Client(1);
        let tag = Tag { client, generation: 5 };
        let at: ScreenRect = rect(4.0, 32.0, 859.0, 1081.0);
        let floating = rect(224.0, 95.0, 600.0, 400.0);
        let ask = on_screen(
            tag,
            Moment::FrameZero,
            &[(wid(1), at)],
            &[(wid(9), floating)],
            &[wid(9)],
        );
        assert_eq!((ask.tag, ask.moment), (tag, Moment::FrameZero));
        let size = at.size;
        let target = PictureTarget {
            window: wid(1),
            server_id: WindowServerId::new(1),
            size,
        };
        assert_eq!(ask.strip, vec![(target, at)]);
        assert_eq!(ask.background.len(), 1);
        assert_eq!(ask.behind, vec![WindowServerId::new(9)]);
        assert_eq!(
            sized_targets(vec![(wid(1), at)]),
            vec![target],
            "warmed at the size it ended"
        );
    }
}
