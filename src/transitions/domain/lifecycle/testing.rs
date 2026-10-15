//! What the lifecycle's tests and the journal's stage with: windows, frames on the built-in display,
//! the strip panning one column, and a coordinator with it under way.

use std::num::NonZeroU32;
use std::time::{Duration, Instant};

use rini_core::ids::WindowId;
use rini_geometry::space::{ScreenPoint, ScreenRect, ScreenSize};

use super::{Coordinator, Event, Kind, StageSummary, Start, TileSummary};

pub(in crate::transitions) fn w(idx: u32) -> WindowId {
    WindowId {
        pid: 1,
        idx: NonZeroU32::new(idx).unwrap(),
    }
}

pub(in crate::transitions) fn r(x: f64, y: f64, w: f64, h: f64) -> ScreenRect {
    ScreenRect::new(ScreenPoint::new(x, y), ScreenSize::new(w, h))
}

pub(in crate::transitions) const DISPLAY: ScreenRect =
    ScreenRect::new(ScreenPoint::new(0.0, 0.0), ScreenSize::new(1728.0, 1117.0));
pub(in crate::transitions) const PLAYS: Duration = Duration::from_millis(350);

pub(in crate::transitions) fn tile(
    window: WindowId,
    start: ScreenRect,
    end: ScreenRect,
) -> TileSummary {
    TileSummary {
        window,
        start,
        end,
        floating: false,
    }
}

/// The strip scrolling one column left: window 1 leaves on the right, window 2 comes in from the
/// left, focused. Window 9 floats behind the strip and stays.
pub(in crate::transitions) fn pan() -> StageSummary {
    let a_start = r(3.0, 35.0, 860.0, 1079.0);
    let a_end = r(866.0, 35.0, 860.0, 1079.0);
    let b_start = r(-860.0, 35.0, 860.0, 1079.0);
    let b_end = r(3.0, 35.0, 860.0, 1079.0);
    let floating = r(224.0, 95.0, 600.0, 400.0);
    StageSummary {
        kind: Kind::Pan,
        tiles: vec![
            tile(w(1), a_start, a_end),
            tile(w(2), b_start, b_end),
            TileSummary {
                window: w(9),
                start: floating,
                end: floating,
                floating: true,
            },
        ],
        final_frames: vec![(w(1), a_end), (w(2), b_end)],
        focus: Some(w(2)),
        losing_focus: Some(w(1)),
        key: Some(w(1)),
        focused_look: None,
        waits: Default::default(),
        start: Start::Immediate,
        duration: PLAYS,
        display: DISPLAY,
        rests: true,
        opaque: Vec::new(),
    }
}

pub(in crate::transitions) fn running(stage: StageSummary, at: Instant) -> Coordinator {
    let mut c = Coordinator::default();
    c.on(Event::Begin(stage), at);
    let g = c.generation();
    c.on(Event::FrameZero(g), at);
    c
}
