//! Telling a window its app zoomed from any other resize.
//!
//! A double-click on a title bar has macOS zoom the window, and Accessibility reports that as a run of
//! ordinary resizes. The double-click, seen by the input tap, is the only evidence, so a resize reads
//! as a zoom only while one is recent. The requirement is "Full width and height" in `specs/strip.md`.
use std::time::{Duration, Instant};

/// The last double-click, and whether a zoom has answered it yet.
#[derive(Debug, Default)]
pub struct DoubleClick {
    at: Option<Instant>,
    answered: bool,
}

/// What a tiled window's resize means while a double-click is recent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Zoom {
    /// The app zoomed the window, or zoomed it back: rini's full width toggles.
    Toggle,
    /// A frame of the zoom's animation, or one after rini has answered it: nothing is adopted.
    InFlight,
}

impl DoubleClick {
    /// Long enough to cover a zoom's animation; a resize later than this is not the zoom's.
    const BELIEVED_FOR: Duration = Duration::from_secs(1);

    pub fn clicked(&mut self, now: Instant) {
        *self = Self { at: Some(now), answered: false };
    }

    /// This double-click's zoom has toggled full width, so the rest of its animation must not.
    pub fn answered(&mut self) {
        self.answered = true;
    }

    /// Whether a double-click is recent enough to explain a resize at all.
    pub fn is_recent(&self, now: Instant) -> bool {
        self.at.is_some_and(|at| now.saturating_duration_since(at) < Self::BELIEVED_FOR)
    }

    /// What a tiled window's resize means, asked only while the double-click `is_recent`.
    ///
    /// `lands` is whether the new frame is where a zoom ends: filling the tiling area, or any size at
    /// all for a window already at full width, which a second zoom takes back out.
    pub fn read(&self, lands: bool) -> Zoom {
        if lands && !self.answered {
            Zoom::Toggle
        } else {
            Zoom::InFlight
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clicked_at(at: Instant) -> DoubleClick {
        let mut click = DoubleClick::default();
        click.clicked(at);
        click
    }

    #[test]
    fn without_a_double_click_a_resize_is_only_a_resize() {
        assert!(!DoubleClick::default().is_recent(Instant::now()));
    }

    #[test]
    fn a_recent_double_click_turns_the_landing_frame_into_a_toggle_and_the_rest_into_flight() {
        let at = Instant::now();
        let click = clicked_at(at);
        assert!(click.is_recent(at + Duration::from_millis(300)));
        assert_eq!(click.read(true), Zoom::Toggle);
        assert_eq!(click.read(false), Zoom::InFlight);
    }

    #[test]
    fn a_double_click_is_believed_for_a_second_and_no_longer() {
        let at = Instant::now();
        let click = clicked_at(at);
        assert!(click.is_recent(at));
        assert!(click.is_recent(at + Duration::from_millis(999)));
        assert!(!click.is_recent(at + Duration::from_secs(1)));
        assert!(!click.is_recent(at + Duration::from_secs(5)));
    }

    #[test]
    fn one_double_click_toggles_once_and_the_next_one_toggles_again() {
        let at = Instant::now();
        let mut click = clicked_at(at);
        click.answered();
        assert!(click.is_recent(at + Duration::from_millis(200)));
        assert_eq!(
            click.read(true),
            Zoom::InFlight,
            "the rest of the animation, and rini's own write, must not toggle back"
        );
        click.clicked(at + Duration::from_secs(3));
        assert!(click.is_recent(at + Duration::from_millis(3200)));
        assert_eq!(click.read(true), Zoom::Toggle);
    }
}
