use objc2_core_foundation::{CGPoint, CGRect};

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

    fn intersection_area(a: CGRect, b: CGRect) -> f64 {
        let width = (a.max().x.min(b.max().x) - a.origin.x.max(b.origin.x)).max(0.0);
        let height = (a.max().y.min(b.max().y) - a.origin.y.max(b.origin.y)).max(0.0);
        width * height
    }

    pub fn calculate(
        screen: CGRect,
        window: CGRect,
        preferred_corner: HideCorner,
        other_screens: &[CGRect],
    ) -> CGRect {
        let preferred = Self::rect_for_corner(screen, window, preferred_corner);
        let alternate = Self::rect_for_corner(screen, window, preferred_corner.opposite());
        let overlap = |candidate| {
            other_screens
                .iter()
                .map(|other| Self::intersection_area(candidate, *other))
                .sum::<f64>()
        };
        if overlap(alternate) < overlap(preferred) {
            alternate
        } else {
            preferred
        }
    }

    /// Whether a frame shows nothing the eye can use on the display.
    ///
    /// A strip position thousands of points along the strip is off screen, and macOS will not honour it:
    /// asked for x = -12396 it places the window with 40pt showing instead, which is the row of slivers
    /// down each edge. Those frames get a corner park instead, which macOS does honour at 1pt.
    ///
    /// A park itself keeps a sliver on screen, so "any intersection at all" misclassified every
    /// parked window as visible — and a parked window animated back in then travelled from its park
    /// in the bottom corner instead of entering from the strip's edge. Off screen therefore means:
    /// no intersection, or a sliver within the park clamp in both axes. Live parks measure up to
    /// 32pt visible (bottom parks at y = display height - 32), and macOS itself will not push a
    /// window further off an edge than 40pt, so nothing genuinely meant to be seen shows 40pt or
    /// less in BOTH axes — a column peeking in at an edge shows its full height.
    pub fn is_off_screen(screen: CGRect, window: CGRect) -> bool {
        const PARK_CLAMP_PX: f64 = 40.0;
        if Self::intersection_area(window, screen) <= 0.0 {
            return true;
        }
        let visible_width =
            (window.max().x.min(screen.max().x) - window.origin.x.max(screen.origin.x)).max(0.0);
        let visible_height =
            (window.max().y.min(screen.max().y) - window.origin.y.max(screen.origin.y)).max(0.0);
        visible_width <= PARK_CLAMP_PX && visible_height <= PARK_CLAMP_PX
    }

    /// Where a parked window should start an animation that brings it back on screen.
    ///
    /// Its real frame is a corner, so animating from there flies it in diagonally from the bottom of the
    /// display. It belongs to the strip, so it comes back the way it left: same row as its destination, just
    /// past the edge it was parked against.
    pub fn entry_frame(park: CGRect, destination: CGRect, display: CGRect) -> CGRect {
        let from_the_left = park.mid().x < display.mid().x;
        let x = if from_the_left {
            display.origin.x - destination.size.width
        } else {
            display.max().x
        };
        CGRect::new(CGPoint::new(x, destination.origin.y), destination.size)
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

    /// A strip coordinate thousands of points along the strip has nothing on screen, which is the frame
    /// macOS refuses and turns into a 40pt sliver. Measured strip positions from this desktop.
    #[test]
    fn a_strip_position_far_along_the_strip_is_off_screen() {
        let screen = rect(0.0, 0.0, 1728.0, 1117.0);
        assert!(HiddenWindowPlacement::is_off_screen(screen, rect(-12396.0, 32.0, 859.0, 1081.0)));
        assert!(HiddenWindowPlacement::is_off_screen(screen, rect(15848.0, 32.0, 1720.0, 1081.0)));
        assert!(HiddenWindowPlacement::is_off_screen(screen, rect(-859.0, 32.0, 859.0, 1081.0)));
    }

    /// A column peeking in at the edge is meant to be seen, so it keeps the position the layout gave it.
    #[test]
    fn a_corner_park_with_a_sliver_showing_is_off_screen() {
        let screen = rect(0.0, 0.0, 1728.0, 1117.0);
        // The measured park positions. 1pt corner parks, and the live ones at y=1085 showing a
        // 32pt band along the bottom: both must read as off screen or a parked window animated
        // back in travels from the bottom corner instead of entering from the strip's edge.
        assert!(HiddenWindowPlacement::is_off_screen(screen, rect(1727.0, 1116.0, 859.0, 1081.0)));
        assert!(HiddenWindowPlacement::is_off_screen(screen, rect(-858.0, 1116.0, 859.0, 1081.0)));
        assert!(HiddenWindowPlacement::is_off_screen(screen, rect(1727.0, 1085.0, 1720.0, 1081.0)));
        assert!(HiddenWindowPlacement::is_off_screen(screen, rect(-858.0, 1085.0, 859.0, 1081.0)));
    }

    #[test]
    fn a_column_with_any_part_on_screen_is_left_alone() {
        let screen = rect(0.0, 0.0, 1728.0, 1117.0);
        assert!(!HiddenWindowPlacement::is_off_screen(screen, rect(-800.0, 32.0, 859.0, 1081.0)));
        assert!(!HiddenWindowPlacement::is_off_screen(screen, rect(1700.0, 32.0, 859.0, 1081.0)));
        assert!(!HiddenWindowPlacement::is_off_screen(screen, rect(4.0, 32.0, 859.0, 1081.0)));
    }

    /// Parked windows come back the way they left. Without this the window flies up from the bottom corner,
    /// because that is where its real frame is.
    #[test]
    fn a_window_parked_on_the_left_comes_back_from_the_left() {
        let display = rect(0.0, 0.0, 1728.0, 1117.0);
        let park = rect(-858.0, 1116.0, 859.0, 1081.0);
        let destination = rect(4.0, 32.0, 859.0, 1081.0);
        let entry = HiddenWindowPlacement::entry_frame(park, destination, display);
        assert_eq!(entry.origin.x, -859.0, "just past the left edge");
        assert_eq!(entry.origin.y, 32.0, "on its destination's row, not at the bottom");
        assert_eq!(entry.size, destination.size);
    }

    #[test]
    fn a_window_parked_on_the_right_comes_back_from_the_right() {
        let display = rect(0.0, 0.0, 1728.0, 1117.0);
        let park = rect(1727.0, 1116.0, 859.0, 1081.0);
        let destination = rect(865.0, 32.0, 859.0, 1081.0);
        let entry = HiddenWindowPlacement::entry_frame(park, destination, display);
        assert_eq!(entry.origin.x, 1728.0, "just past the right edge");
        assert_eq!(entry.origin.y, 32.0);
    }

    /// A display that is not at the origin: the edges are the display's own, not the global zero.
    #[test]
    fn entry_is_relative_to_the_display_it_happens_on() {
        let display = rect(-670.0, -1692.0, 3008.0, 1692.0);
        let park = rect(2337.0, -1.0, 859.0, 1081.0);
        let destination = rect(-666.0, -1660.0, 859.0, 1081.0);
        let entry = HiddenWindowPlacement::entry_frame(park, destination, display);
        assert_eq!(entry.origin.x, 2338.0, "past the right edge of THAT display");
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
}
