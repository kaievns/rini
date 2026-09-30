use objc2_core_foundation::CGRect;

pub fn compute_tiling_area(screen: CGRect, gaps: &crate::layout::settings::GapSettings) -> CGRect {
    use objc2_core_foundation::{CGPoint, CGSize};

    use rini_geometry::Round;
    if gaps.outer.top == 0.0
        && gaps.outer.left == 0.0
        && gaps.outer.bottom == 0.0
        && gaps.outer.right == 0.0
    {
        screen
    } else {
        CGRect {
            origin: CGPoint {
                x: screen.origin.x + gaps.outer.left,
                y: screen.origin.y + gaps.outer.top,
            },
            size: CGSize {
                width: (screen.size.width - gaps.outer.left - gaps.outer.right).max(0.0),
                height: (screen.size.height - gaps.outer.top - gaps.outer.bottom).max(0.0),
            },
        }
        .round()
    }
}

#[cfg(test)]
mod tests {
    use objc2_core_foundation::{CGPoint, CGSize};

    use super::*;

    #[test]
    fn zero_outer_gaps_return_the_screen_untouched() {
        let screen = CGRect::new(CGPoint::new(0.3, 34.0), CGSize::new(1728.7, 1083.0));
        let area = compute_tiling_area(screen, &Default::default());
        assert_eq!(area.origin.x, 0.3, "no rounding when nothing is inset");
    }

    #[test]
    fn outer_gaps_inset_each_edge_and_never_go_negative() {
        let mut gaps = crate::layout::settings::GapSettings::default();
        gaps.outer.top = 10.0;
        gaps.outer.left = 4.0;
        gaps.outer.bottom = 6.0;
        gaps.outer.right = 4.0;
        let screen = CGRect::new(CGPoint::new(0.0, 34.0), CGSize::new(1728.0, 1083.0));
        let area = compute_tiling_area(screen, &gaps);
        assert_eq!((area.origin.x, area.origin.y), (4.0, 44.0));
        assert_eq!((area.size.width, area.size.height), (1720.0, 1067.0));

        gaps.outer.left = 2000.0;
        let clamped = compute_tiling_area(screen, &gaps);
        assert_eq!(clamped.size.width, 0.0);
    }
}
