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

/// Whether `frame` is at least the size of the tiling area both ways, as a window its app zoomed to
/// the screen is. Size alone: a zoom covers the gaps and the bar rather than sitting inside them.
pub fn fills_tiling_area(frame: CGRect, tiling: CGRect) -> bool {
    // The area is rounded and apps round their own frames, so a point either way is the same size.
    const TOLERANCE: f64 = 2.0;
    frame.size.width >= tiling.size.width - TOLERANCE
        && frame.size.height >= tiling.size.height - TOLERANCE
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

    #[test]
    fn a_frame_fills_the_tiling_area_when_it_is_at_least_its_size_both_ways() {
        let tiling = CGRect::new(CGPoint::new(8.0, 42.0), CGSize::new(1424.0, 850.0));
        let visible = CGRect::new(CGPoint::new(0.0, 34.0), CGSize::new(1440.0, 866.0));
        assert!(fills_tiling_area(tiling, tiling));
        assert!(fills_tiling_area(visible, tiling), "a zoom covers the gaps");
        let rounded = CGRect::new(tiling.origin, CGSize::new(1422.5, 849.0));
        assert!(fills_tiling_area(rounded, tiling), "within rounding of the area");
    }

    #[test]
    fn a_column_or_a_stacked_row_does_not_fill_the_tiling_area() {
        let tiling = CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(1440.0, 900.0));
        let two_thirds = CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(960.0, 900.0));
        let almost = CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(1436.0, 900.0));
        let half_height = CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(1440.0, 446.0));
        assert!(!fills_tiling_area(two_thirds, tiling));
        assert!(!fills_tiling_area(almost, tiling));
        assert!(!fills_tiling_area(half_height, tiling));
    }
}
