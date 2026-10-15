//! The windows, frames and rounds the tests of every watch are written in.

use std::time::{Duration, Instant};

use rini_core::ids::{WindowId, WindowServerId};
use rini_geometry::space::{ScreenPoint, ScreenRect, ScreenSize};

use super::{
    CHANGE_EVERY, CHANGE_TRIES, Capture, Client, END_SLACK, LIVE_EVERY, Moment, OnScreen, Outcome,
    Prints, Seen, Set, Settle, Tag, Watch, WatchId, Watches, Why,
};
use crate::pictures::domain::target::PictureTarget;

pub(super) const LAPTOP: ScreenRect = rect(0.0, 0.0, 1728.0, 1117.0);

pub(super) const fn rect(x: f64, y: f64, w: f64, h: f64) -> ScreenRect {
    ScreenRect::new(ScreenPoint::new(x, y), ScreenSize::new(w, h))
}

pub(super) fn wid(idx: u32) -> WindowId { WindowId::new(1, idx) }

pub(super) fn target(idx: u32) -> PictureTarget {
    PictureTarget {
        window: wid(idx),
        server_id: WindowServerId::new(idx),
        size: ScreenSize::new(859.0, 1081.0),
    }
}

pub(super) fn tag(generation: u64) -> Tag { Tag { client: Client(1), generation } }

pub(super) const LEFT: ScreenRect = rect(4.0, 32.0, 859.0, 1081.0);

pub(super) const RIGHT: ScreenRect = rect(865.0, 32.0, 859.0, 1081.0);

/// Parked past the right edge.
pub(super) const PARKED: ScreenRect = rect(1727.0, 32.0, 859.0, 1081.0);

pub(super) fn ms(n: u64) -> Duration { Duration::from_millis(n) }

/// The windows a round looks at, by set.
pub(super) fn looked(sets: &[Set]) -> Vec<Vec<WindowId>> {
    sets.iter()
        .map(|set| set.looks.iter().map(|look| look.target.window).collect())
        .collect()
}

/// One round: what `next` hands out, and what `seen` makes of each window being where `at` says,
/// captured and taken by the cache with the band print `band` says (`None`: not captured), on the
/// laptop.
pub(super) fn round(
    watches: &mut Watches,
    now: Instant,
    at: impl Fn(WindowId) -> Option<ScreenRect>,
    band: impl Fn(WindowId) -> Option<u8>,
) -> Option<(Vec<Set>, Outcome)> {
    round_of(watches, now, at, |window| {
        band(window).map_or(Capture::Missed, taken)
    })
}

/// One round in which what came of each capture is what `capture` says.
pub(super) fn round_of(
    watches: &mut Watches,
    now: Instant,
    at: impl Fn(WindowId) -> Option<ScreenRect>,
    capture: impl Fn(WindowId) -> Capture,
) -> Option<(Vec<Set>, Outcome)> {
    let sets = watches.next(now)?;
    let seen: Vec<Seen> = sets
        .iter()
        .flat_map(|set| &set.looks)
        .map(|look| {
            let window = look.target.window;
            let frame = at(window);
            let captured = frame.is_some_and(|f| look.accepts.iter().any(|a| a.accepts(f, LAPTOP)));
            Seen {
                window,
                frame,
                capture: if captured {
                    capture(window)
                } else {
                    Capture::Missed
                },
            }
        })
        .collect();
    let outcome = watches.seen(now, &seen, Some(LAPTOP));
    Some((sets, outcome))
}

/// A capture the cache took with band print `band`; thumbprints 50 levels apart per step, past the
/// settle tolerance.
pub(super) fn taken(band: u8) -> Capture {
    Capture::Taken(Prints {
        band: vec![band],
        thumb: Some(vec![band.wrapping_mul(50)]),
    })
}

pub(super) fn delivered(outcome: &Outcome) -> Vec<(Why, Vec<WindowId>)> {
    outcome
        .deliveries
        .iter()
        .map(|d| (d.why, d.windows.iter().map(|(w, _)| *w).collect()))
        .collect()
}

pub(super) fn frame_zero(
    strip: &[(u32, ScreenRect)],
    background: &[(u32, ScreenRect)],
) -> OnScreen {
    OnScreen {
        tag: tag(1),
        moment: Moment::FrameZero,
        strip: strip.iter().map(|(idx, at)| (target(*idx), *at)).collect(),
        background: background.iter().map(|(idx, at)| (target(*idx), *at)).collect(),
        behind: vec![WindowServerId::new(9)],
    }
}

pub(super) fn landing(windows: &[(u32, ScreenRect)], settle: Settle, until: Instant) -> Watch {
    landing_of(tag(1), vec![WindowServerId::new(9)], windows, settle, until)
}

pub(super) fn landing_of(
    tag: Tag,
    behind: Vec<WindowServerId>,
    windows: &[(u32, ScreenRect)],
    settle: Settle,
    until: Instant,
) -> Watch {
    Watch::Landing {
        tag,
        windows: windows.iter().map(|(idx, end)| (target(*idx), *end)).collect(),
        behind,
        slack: END_SLACK,
        settle,
        until,
    }
}

pub(super) fn change(idx: u32, end: ScreenRect) -> Watch {
    Watch::Change {
        tag: tag(1),
        window: target(idx),
        end,
        losing: None,
        behind: vec![WindowServerId::new(9)],
        every: CHANGE_EVERY,
        tries: CHANGE_TRIES,
    }
}

/// Window 2 gaining focus where it ends at `RIGHT`, window 1 losing it where it ends at `losing`.
pub(super) fn changing(losing: ScreenRect) -> Watch {
    match change(2, RIGHT) {
        Watch::Change {
            tag,
            window,
            end,
            behind,
            every,
            tries,
            ..
        } => Watch::Change {
            tag,
            window,
            end,
            losing: Some((target(1), losing)),
            behind,
            every,
            tries,
        },
        _ => unreachable!(),
    }
}

pub(super) fn live(windows: &[u32], after: Option<WatchId>, initial: Option<Instant>) -> Watch {
    Watch::Live {
        tag: tag(1),
        windows: windows.iter().map(|idx| target(*idx)).collect(),
        every: LIVE_EVERY,
        after,
        initial,
    }
}
