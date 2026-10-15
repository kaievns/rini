use objc2_core_foundation::{CGPoint, CGRect};
use rini_geometry::CGRectExt;
use rini_geometry::space::{FromCg, ScreenRect};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HideCorner {
    BottomLeft,
    #[default]
    BottomRight,
}

impl HideCorner {
    pub fn opposite(self) -> Self {
        match self {
            Self::BottomLeft => Self::BottomRight,
            Self::BottomRight => Self::BottomLeft,
        }
    }
}

/// Pure geometry used to place inactive-workspace windows just offscreen.
pub struct HiddenWindowPlacement;

impl HiddenWindowPlacement {
    const REVEAL_PX: f64 = 1.0;
    const VISIBLE_THRESHOLD_PX: f64 = 3.0;

    fn rect_for_corner(screen: CGRect, window: CGRect, corner: HideCorner) -> CGRect {
        let x = match corner {
            HideCorner::BottomLeft => screen.origin.x - window.size.width + Self::REVEAL_PX,
            HideCorner::BottomRight => screen.max().x - Self::REVEAL_PX,
        };
        CGRect::new(CGPoint::new(x, screen.max().y - Self::REVEAL_PX), window.size)
    }

    pub fn calculate(
        screen: CGRect,
        window: CGRect,
        preferred_corner: HideCorner,
        other_screens: &[CGRect],
    ) -> CGRect {
        let preferred = Self::rect_for_corner(screen, window, preferred_corner);
        let alternate = Self::rect_for_corner(screen, window, preferred_corner.opposite());
        let overlap = |candidate: CGRect| {
            other_screens
                .iter()
                .map(|other| candidate.intersection(other).area())
                .sum::<f64>()
        };
        if overlap(alternate) < overlap(preferred) {
            alternate
        } else {
            preferred
        }
    }

    /// `calculate` against every attached display, `screen` among them.
    pub fn calculate_among(
        screen: CGRect,
        window: CGRect,
        preferred_corner: HideCorner,
        all_screens: &[CGRect],
    ) -> CGRect {
        Self::calculate(screen, window, preferred_corner, &Self::others(screen, all_screens))
    }

    /// `is_hidden` against every attached display, `screen` among them.
    pub fn is_hidden_among(screen: CGRect, window: CGRect, all_screens: &[CGRect]) -> bool {
        Self::is_hidden(screen, window, &Self::others(screen, all_screens))
    }

    fn others(screen: CGRect, all_screens: &[CGRect]) -> Vec<CGRect> {
        all_screens.iter().copied().filter(|other| *other != screen).collect()
    }

    /// `rini_geometry::is_off_screen`; see `src/layout/docs/strip.md` "Parking".
    pub fn is_off_screen(screen: CGRect, window: CGRect) -> bool {
        rini_geometry::is_off_screen(ScreenRect::from_cg(screen), ScreenRect::from_cg(window))
    }

    /// The least of a floating window that must show on each axis for its frame to be worth keeping.
    ///
    /// About a title bar's grab area. Not a guess at what looks nice: below this there is nothing left
    /// to click, so the user cannot drag the window back either.
    const FLOATING_USABLE_PX: f64 = 64.0;

    /// Whether a remembered floating frame still puts enough of the window on screen to use it.
    ///
    /// EITHER axis is enough to condemn a frame, and that is the whole reason this is not one of the two
    /// predicates above. Both of those ask "is this a parked TILED column", and both require a sliver in
    /// BOTH axes, because a column peeking in at the edge of the strip shows its full height and is
    /// legitimately on screen. A park always shows most of its height, so a floating window parked in a
    /// corner passes both of them.
    ///
    /// Which is how three floating windows ended up stranded. Their measured frames showed 1pt of width
    /// against 28pt, 44pt and 52pt of height, so `is_hidden` (3pt) and `is_off_screen` (40pt) both
    /// called them real positions — and the floating path writes the frame back as the window's own
    /// position every time the workspace is arranged, so reading it as real once is permanent.
    pub fn floating_frame_is_usable(screen: CGRect, window: CGRect) -> bool {
        let visible_width =
            (window.max().x.min(screen.max().x) - window.origin.x.max(screen.origin.x)).max(0.0);
        let visible_height =
            (window.max().y.min(screen.max().y) - window.origin.y.max(screen.origin.y)).max(0.0);
        visible_width >= Self::FLOATING_USABLE_PX && visible_height >= Self::FLOATING_USABLE_PX
    }

    pub fn is_hidden(screen: CGRect, window: CGRect, other_screens: &[CGRect]) -> bool {
        [HideCorner::BottomLeft, HideCorner::BottomRight]
            .into_iter()
            .any(|corner| Self::calculate(screen, window, corner, other_screens) == window)
            || {
                let visible_width = (window.max().x.min(screen.max().x)
                    - window.origin.x.max(screen.origin.x))
                .max(0.0);
                let visible_height = (window.max().y.min(screen.max().y)
                    - window.origin.y.max(screen.origin.y))
                .max(0.0);
                visible_width <= Self::VISIBLE_THRESHOLD_PX
                    && visible_height <= Self::VISIBLE_THRESHOLD_PX
            }
    }
}

#[cfg(test)]
mod tests {
    use objc2_core_foundation::{CGPoint, CGSize};

    use super::*;

    fn rect(x: f64, y: f64, width: f64, height: f64) -> CGRect {
        CGRect::new(CGPoint::new(x, y), CGSize::new(width, height))
    }

    /// The measured frames of the three floating windows found stranded, each showing 1pt of width.
    #[test]
    fn a_floating_frame_parked_in_a_corner_is_not_usable() {
        let screen = rect(0.0, 32.0, 1728.0, 1085.0);
        let parked = [
            rect(1727.0, 1089.0, 900.0, 1079.0),
            rect(1727.0, 1065.0, 723.0, 884.0),
            rect(1727.0, 1073.0, 1280.0, 960.0),
        ];

        for frame in parked {
            assert!(
                !HiddenWindowPlacement::floating_frame_is_usable(screen, frame),
                "{frame:?} leaves nothing to grab"
            );
        }
    }

    /// Why this is not one of the other two predicates: both of them call these same frames real
    /// positions, and for tiled columns they are right to. Remove the one-axis rule above and this test
    /// is what fails.
    #[test]
    fn the_two_axis_tests_both_accept_a_parked_float() {
        let screen = rect(0.0, 32.0, 1728.0, 1085.0);
        let parked = rect(1727.0, 1065.0, 723.0, 884.0);

        assert!(!HiddenWindowPlacement::is_hidden(screen, parked, &[]));
        assert!(!HiddenWindowPlacement::is_off_screen(screen, parked));
        assert!(!HiddenWindowPlacement::floating_frame_is_usable(screen, parked));
    }

    /// A floating window the user dragged half off an edge is theirs to keep.
    #[test]
    fn a_floating_frame_hanging_off_an_edge_is_still_usable() {
        let screen = rect(0.0, 32.0, 1728.0, 1085.0);

        assert!(HiddenWindowPlacement::floating_frame_is_usable(
            screen,
            rect(1400.0, 200.0, 900.0, 600.0)
        ));
        assert!(HiddenWindowPlacement::floating_frame_is_usable(
            screen,
            rect(-400.0, 200.0, 900.0, 600.0)
        ));
    }

    #[test]
    fn a_floating_frame_entirely_off_the_display_is_not_usable() {
        let screen = rect(0.0, 32.0, 1728.0, 1085.0);
        assert!(!HiddenWindowPlacement::floating_frame_is_usable(
            screen,
            rect(4000.0, 200.0, 900.0, 600.0)
        ));
    }

    /// A strip coordinate thousands of points along the strip has nothing on screen, which is the frame
    /// macOS refuses and turns into a 40pt sliver. Measured strip positions from this desktop.
    #[test]
    fn a_strip_position_far_along_the_strip_is_off_screen() {
        let screen = rect(0.0, 0.0, 1728.0, 1117.0);
        assert!(HiddenWindowPlacement::is_off_screen(
            screen,
            rect(-12396.0, 32.0, 859.0, 1081.0)
        ));
        assert!(HiddenWindowPlacement::is_off_screen(
            screen,
            rect(15848.0, 32.0, 1720.0, 1081.0)
        ));
        assert!(HiddenWindowPlacement::is_off_screen(
            screen,
            rect(-859.0, 32.0, 859.0, 1081.0)
        ));
    }

    /// A column peeking in at the edge is meant to be seen, so it keeps the position the layout gave it.
    #[test]
    fn a_corner_park_with_a_sliver_showing_is_off_screen() {
        let screen = rect(0.0, 0.0, 1728.0, 1117.0);
        // Corner parks and the live y=1085 parks must both read as off screen (`src/layout/docs/strip.md`).
        assert!(HiddenWindowPlacement::is_off_screen(
            screen,
            rect(1727.0, 1116.0, 859.0, 1081.0)
        ));
        assert!(HiddenWindowPlacement::is_off_screen(
            screen,
            rect(-858.0, 1116.0, 859.0, 1081.0)
        ));
        assert!(HiddenWindowPlacement::is_off_screen(
            screen,
            rect(1727.0, 1085.0, 1720.0, 1081.0)
        ));
        assert!(HiddenWindowPlacement::is_off_screen(
            screen,
            rect(-858.0, 1085.0, 859.0, 1081.0)
        ));
    }

    #[test]
    fn a_column_with_any_part_on_screen_is_left_alone() {
        let screen = rect(0.0, 0.0, 1728.0, 1117.0);
        assert!(!HiddenWindowPlacement::is_off_screen(
            screen,
            rect(-800.0, 32.0, 859.0, 1081.0)
        ));
        assert!(!HiddenWindowPlacement::is_off_screen(
            screen,
            rect(1700.0, 32.0, 859.0, 1081.0)
        ));
        assert!(!HiddenWindowPlacement::is_off_screen(
            screen,
            rect(4.0, 32.0, 859.0, 1081.0)
        ));
    }

    #[test]
    fn anchors_to_requested_corner() {
        let hidden = HiddenWindowPlacement::calculate(
            rect(0.0, 0.0, 1000.0, 800.0),
            rect(10.0, 20.0, 200.0, 100.0),
            HideCorner::BottomRight,
            &[],
        );
        assert_eq!(hidden, rect(999.0, 799.0, 200.0, 100.0));
    }

    #[test]
    fn avoids_an_adjacent_monitor() {
        let screen = rect(0.0, 0.0, 1000.0, 800.0);
        let hidden = HiddenWindowPlacement::calculate(
            screen,
            rect(0.0, 0.0, 200.0, 100.0),
            HideCorner::BottomRight,
            &[rect(1000.0, 0.0, 1000.0, 800.0)],
        );
        assert_eq!(hidden.origin.x, -199.0);
    }

    #[test]
    fn hidden_position_uses_corner_anchor_while_hiding_offscreen() {
        let screen = rect(0.0, 0.0, 100.0, 100.0);
        let frame = rect(20.0, 37.0, 30.0, 20.0);

        let hidden =
            HiddenWindowPlacement::calculate_among(screen, frame, HideCorner::BottomRight, &[screen]);

        assert_eq!(hidden.origin.y, screen.max().y - 1.0);
        assert_eq!(hidden.origin.x, screen.max().x - 1.0);
    }

    #[test]
    fn hidden_position_flips_sides_to_avoid_neighboring_monitor_overlap() {
        let primary = rect(0.0, 0.0, 100.0, 100.0);
        let right_neighbor = rect(100.0, 0.0, 100.0, 100.0);
        let frame = rect(20.0, 25.0, 30.0, 20.0);

        let hidden = HiddenWindowPlacement::calculate_among(
            primary,
            frame,
            HideCorner::BottomRight,
            &[primary, right_neighbor],
        );

        assert_eq!(hidden.origin.y, primary.max().y - 1.0);
        assert_eq!(hidden.origin.x, primary.origin.x - frame.size.width + 1.0);
    }
}
