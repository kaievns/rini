//! What a capture is asked for: the port the reactor, the flight engine and the switcher name a window
//! to picture through, the id a window rini does not manage is pictured under, which of those windows
//! a warm pictures, and which of their pictures go.

use std::collections::HashSet;

use rini_core::ids::{WindowId, WindowServerId};
use rini_geometry::space::{ScreenRect, ScreenSize};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PictureTarget {
    pub window: WindowId,
    pub server_id: WindowServerId,
    /// The window's full size, as the layout intends it.
    pub size: ScreenSize,
}

/// The least a window must show of itself on the display, each way, for a warm to picture it. A window
/// parked at the corner keeps its full size and shows a sliver there.
pub const WARM_MIN_SHOWN: f64 = 100.0;

/// A stable [`WindowId`] for a window rini does not manage, derived from its window server id: a border
/// window, or any window the debug commands picture. Pid 0 keeps it clear of real ids.
pub fn synthetic_window_id(server_id: WindowServerId) -> WindowId {
    WindowId {
        pid: 0,
        idx: std::num::NonZeroU32::new(server_id.as_u32().max(1)).unwrap(),
    }
}

pub fn is_synthetic(window: WindowId) -> bool { window.pid == 0 }

/// The window server ids of the windows among `pictured` that rini manages: every id but a synthetic
/// one names a managed window by its server id.
pub fn managed_server_ids(pictured: impl Iterator<Item = WindowId>) -> HashSet<u32> {
    pictured.filter(|window| !is_synthetic(*window)).map(|window| window.idx.get()).collect()
}

/// What a warm of `display` pictures of the windows `listed` on it: each that is not `managed` and
/// shows at least [`WARM_MIN_SHOWN`] of itself each way, under its synthetic id. A managed window is
/// pictured under its own id, and once is enough.
pub fn warm_targets(
    listed: &[(WindowServerId, ScreenRect)],
    display: ScreenRect,
    managed: &HashSet<u32>,
) -> Vec<PictureTarget> {
    listed
        .iter()
        .filter(|(server_id, _)| !managed.contains(&server_id.as_u32()))
        .filter(|(_, frame)| {
            frame
                .intersection(&display)
                .is_some_and(|shown| shown.size.width >= WARM_MIN_SHOWN && shown.size.height >= WARM_MIN_SHOWN)
        })
        .map(|&(server_id, frame)| PictureTarget {
            window: synthetic_window_id(server_id),
            server_id,
            size: frame.size,
        })
        .collect()
}

/// The synthetic pictures to let go of among those held.
#[derive(Debug, Default, PartialEq)]
pub struct Unwanted {
    /// Of windows the window server no longer has: everything known of them goes.
    pub gone: Vec<WindowId>,
    /// Of windows rini manages, pictured under their own ids as well: only the second picture goes.
    pub twice: Vec<WindowId>,
}

/// Which of the synthetic ids among `held`, every window pictured or owed a picture, to let go of,
/// given which of their server ids the window server still has (`alive`).
pub fn unwanted(held: &[WindowId], alive: &HashSet<u32>) -> Unwanted {
    let managed = managed_server_ids(held.iter().copied());
    let mut unwanted = Unwanted::default();
    for &window in held.iter().filter(|window| is_synthetic(**window)) {
        let server_id = window.idx.get();
        if !alive.contains(&server_id) {
            unwanted.gone.push(window);
        } else if managed.contains(&server_id) {
            unwanted.twice.push(window);
        }
    }
    unwanted
}

#[cfg(test)]
mod tests {
    use rini_geometry::space::ScreenPoint;

    use super::*;

    /// The built-in display the measurements were taken on, in points.
    fn display() -> ScreenRect { rect(0.0, 0.0, 1728.0, 1117.0) }

    fn rect(x: f64, y: f64, width: f64, height: f64) -> ScreenRect {
        ScreenRect::new(ScreenPoint::new(x, y), ScreenSize::new(width, height))
    }

    fn server(id: u32) -> WindowServerId { WindowServerId::new(id) }

    fn warmed(listed: &[(WindowServerId, ScreenRect)], managed: &[u32]) -> Vec<WindowId> {
        warm_targets(listed, display(), &managed.iter().copied().collect())
            .into_iter()
            .map(|target| target.window)
            .collect()
    }

    #[test]
    fn a_warm_pictures_an_unmanaged_window_on_screen_under_its_synthetic_id_at_its_full_size() {
        let frame = rect(224.0, 95.0, 1280.0, 960.0);
        let targets = warm_targets(&[(server(9003), frame)], display(), &HashSet::new());
        assert_eq!(
            targets,
            vec![PictureTarget {
                window: synthetic_window_id(server(9003)),
                server_id: server(9003),
                size: frame.size,
            }]
        );
    }

    #[test]
    fn a_warm_never_pictures_a_window_rini_manages() {
        assert!(warmed(&[(server(245), rect(3.0, 35.0, 1722.0, 1079.0))], &[245]).is_empty());
    }

    /// Measured 2026-10-06: the parked windows sat at x 1727, y 1065-1089, at their full sizes.
    #[test]
    fn a_warm_never_pictures_a_window_through_the_sliver_it_shows_parked() {
        let parked = [
            (server(75), rect(1727.0, 1085.0, 1722.0, 1079.0)),
            (server(76), rect(1727.0, 1065.0, 860.0, 1079.0)),
        ];
        assert!(warmed(&parked, &[]).is_empty());
    }

    #[test]
    fn a_warm_pictures_a_window_showing_enough_of_itself_each_way() {
        let peeking = (server(1), rect(1728.0 - WARM_MIN_SHOWN, 35.0, 860.0, 1079.0));
        let narrower = (server(2), rect(1728.0 - WARM_MIN_SHOWN + 1.0, 35.0, 860.0, 1079.0));
        let shorter = (server(3), rect(3.0, 1117.0 - WARM_MIN_SHOWN + 1.0, 860.0, 1079.0));
        assert_eq!(warmed(&[peeking, narrower, shorter], &[]), vec![synthetic_window_id(server(1))]);
    }

    #[test]
    fn the_managed_server_ids_are_those_pictured_under_their_own_ids() {
        let pictured = [WindowId::new(82799, 102682), synthetic_window_id(server(9001))];
        assert_eq!(managed_server_ids(pictured.into_iter()), [102682].into_iter().collect());
    }

    #[test]
    fn a_synthetic_picture_of_a_window_gone_from_the_window_server_goes() {
        let gone = synthetic_window_id(server(9001));
        let alive = synthetic_window_id(server(9002));
        let found = unwanted(&[gone, alive], &[9002].into_iter().collect());
        assert_eq!(found, Unwanted { gone: vec![gone], twice: vec![] });
    }

    /// A warm at boot can land before a managed window's own picture is held or asked for.
    #[test]
    fn a_synthetic_picture_of_a_window_also_pictured_under_its_own_id_goes() {
        let managed = WindowId::new(82799, 102682);
        let twice = synthetic_window_id(server(102682));
        let found = unwanted(&[twice, managed], &[102682].into_iter().collect());
        assert_eq!(found, Unwanted { gone: vec![], twice: vec![twice] });
    }

    #[test]
    fn a_window_pictured_under_its_own_id_is_never_let_go_of_here() {
        let managed = WindowId::new(82799, 102682);
        assert_eq!(unwanted(&[managed], &HashSet::new()), Unwanted::default());
    }
}
