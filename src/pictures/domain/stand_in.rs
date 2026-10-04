//! When a window with no picture flies as the dark stand-in rather than as nothing.

use rini_geometry::space::ScreenRect;

/// Whether a window with no picture flies with a stand-in rather than being reserved as a newcomer.
///
/// Only a window the window server knows to be somewhere OFF this display: parked by rini on a
/// workspace nobody is looking at, or on another display. That is every window after a restart, with
/// the picture cache empty, and the reservation a genuinely new window takes left each of them as a
/// hole in the strip — nothing drawn in its slot until a picture landed, which for a parked window is
/// usually never within the flight. A window on this display with no picture is new, and keeps the
/// spawn capture and the reservation.
fn stands_in(server_frame: Option<ScreenRect>, display: ScreenRect) -> bool {
    server_frame.is_some_and(|frame| {
        frame.size.width > 0.0
            && frame.size.height > 0.0
            && rini_geometry::is_off_screen(display, frame)
    })
}

/// What a flight draws for a window: its picture, else the stand-in when `stands_in` says so, else
/// nothing. The server frame and the stand-in are asked for only when there is no picture.
pub fn picture_or_stand_in<S>(
    picture: Option<S>,
    server_frame: impl FnOnce() -> Option<ScreenRect>,
    display: ScreenRect,
    stand_in: impl FnOnce() -> Option<S>,
) -> Option<S> {
    picture.or_else(|| stands_in(server_frame(), display).then(stand_in).flatten())
}

#[cfg(test)]
mod tests {
    use rini_geometry::space::{ScreenPoint, ScreenSize};

    use super::*;

    fn rect(x: f64, y: f64, w: f64, h: f64) -> ScreenRect {
        ScreenRect::new(ScreenPoint::new(x, y), ScreenSize::new(w, h))
    }

    /// The reported hole: after a restart every parked window has no picture. Off this display, it
    /// flies with a stand-in instead of being reserved as a newcomer.
    #[test]
    fn a_parked_window_with_no_picture_stands_in() {
        let display = rect(0.0, 0.0, 1728.0, 1117.0);
        let parked = rect(1727.0, 1085.0, 859.0, 1081.0);
        let elsewhere = rect(-2000.0, -1600.0, 1200.0, 900.0);
        assert!(stands_in(Some(parked), display));
        assert!(stands_in(Some(elsewhere), display), "on another display");
    }

    /// A window on this display with no picture is one that just opened, and keeps the spawn capture.
    #[test]
    fn a_window_on_this_display_is_a_newcomer_not_a_stand_in() {
        let display = rect(0.0, 0.0, 1728.0, 1117.0);
        let spawned = rect(300.0, 200.0, 640.0, 480.0);
        assert!(!stands_in(Some(spawned), display));
        assert!(!stands_in(None, display), "no server frame at all");
        assert!(
            !stands_in(
                Some(rect(1727.0, 1085.0, 0.0, 0.0)),
                display
            ),
            "a zero frame is not a window to stand in for"
        );
    }

    /// A strip movement holds every window of the workspace, and after a restart the parked ones have
    /// no picture: each flies as the stand-in, where it used to be left out and show as a hole.
    #[test]
    fn a_parked_window_with_no_picture_is_drawn_as_the_stand_in() {
        let display = rect(0.0, 0.0, 1728.0, 1117.0);
        let parked = rect(1727.0, 1085.0, 1720.0, 1081.0);
        let on_screen = rect(4.0, 32.0, 1720.0, 1081.0);
        let stand_in = || Some("stand-in");
        assert_eq!(
            picture_or_stand_in(None, || Some(parked), display, stand_in),
            Some("stand-in")
        );
        assert_eq!(
            picture_or_stand_in(None, || Some(on_screen), display, stand_in),
            None,
            "a window on this display with no picture is not stood in for"
        );
        assert_eq!(picture_or_stand_in(None, || None, display, stand_in), None);
        assert_eq!(
            picture_or_stand_in(
                Some("picture"),
                || panic!("no frame is read for a window with a picture"),
                display,
                || panic!("nor a stand-in drawn")
            ),
            Some("picture")
        );
    }
}
