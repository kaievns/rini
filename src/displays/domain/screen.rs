//! What a display is, and the arithmetic over it.
//!
//! `crate::displays::platform::screen` reads NSScreen, CGDisplay, the Dock and the menu bar; every
//! rule it applies to what it read is here. The two are separated on the `System` trait the cache is
//! generic over, which does not yet cover the space lookup, so the cache itself has to stay there.

use std::cmp::Ordering;

use objc2_core_foundation::{CGPoint, CGRect};
use serde::{Deserialize, Serialize};

use rini_core::ids::{ScreenId, SpaceId};
use rini_geometry::CGRectDef;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScreenInfo {
    pub id: ScreenId,
    #[serde(with = "CGRectDef")]
    pub frame: CGRect,
    pub display_uuid: String,
    pub name: Option<String>,
    pub space: Option<SpaceId>,
}

impl ScreenInfo {
    pub fn display_uuid_opt(&self) -> Option<&str> {
        if self.display_uuid.is_empty() {
            None
        } else {
            Some(self.display_uuid.as_str())
        }
    }

    pub fn display_uuid_owned(&self) -> Option<String> {
        self.display_uuid_opt().map(|uuid| uuid.to_string())
    }
}

pub fn menu_bar_inset(hidden: bool, height: f64, notch_height: f64) -> f64 {
    if hidden {
        // An auto-hidden menu bar does not reserve space on displays without a notch.
        // Notched built-in displays must retain their safe-area inset.
        notch_height
    } else {
        // macOS reports the menubar height without the topmost usable pixel; add 1 to
        // avoid leaving a dead strip or placing windows under the bar.
        height + 1.0
    }
}

pub fn rects_intersect(a: &CGRect, b: &CGRect) -> bool {
    let ax2 = a.origin.x + a.size.width;
    let ay2 = a.origin.y + a.size.height;
    let bx2 = b.origin.x + b.size.width;
    let by2 = b.origin.y + b.size.height;

    !(ax2 <= b.origin.x || bx2 <= a.origin.x || ay2 <= b.origin.y || by2 <= a.origin.y)
}

/// Which edge of the screen the Dock is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DockEdge {
    Left,
    Right,
    Bottom,
    /// macOS reported an orientation rini does not recognise; the dock's own shape decides. A wide
    /// dock is along the bottom, a tall one up a side.
    Unknown,
}

/// The part of `raw` a window may actually use: what the menu bar and the Dock do not cover.
///
/// `dock` is `None` when the Dock is not on this display or is not currently shown. A dock that does
/// not overlap what the menu bar left is ignored, and the test is against that rather than against
/// `raw`, so a bar tall enough to cover the dock leaves the dock nothing to take.
///
/// Height and width never go below zero: a dock larger than the display would otherwise produce a
/// negative frame that every later comparison reads as inverted.
pub fn usable_frame(raw: CGRect, menu_bar_inset: f64, dock: Option<(CGRect, DockEdge)>) -> CGRect {
    let mut frame = raw;
    if menu_bar_inset > 0.0 {
        frame.origin.y += menu_bar_inset;
        frame.size.height = (frame.size.height - menu_bar_inset).max(0.0);
    }
    let Some((dock, edge)) = dock.filter(|(dock, _)| rects_intersect(&frame, dock)) else {
        return frame;
    };
    let edge = match edge {
        DockEdge::Unknown if dock.size.width > dock.size.height => DockEdge::Bottom,
        DockEdge::Unknown => DockEdge::Left,
        known => known,
    };
    match edge {
        DockEdge::Left => {
            frame.origin.x += dock.size.width;
            frame.size.width = (frame.size.width - dock.size.width).max(0.0);
        }
        DockEdge::Right => {
            frame.size.width = (frame.size.width - dock.size.width).max(0.0);
        }
        DockEdge::Bottom => {
            frame.size.height = (frame.size.height - dock.size.height).max(0.0);
        }
        DockEdge::Unknown => unreachable!("resolved above"),
    }
    frame
}

/// Converts between Quartz and Cocoa coordinate systems.
#[derive(Clone, Copy, Debug)]
pub struct CoordinateConverter {
    pub(in crate::displays) screen_height: f64,
}

impl Default for CoordinateConverter {
    fn default() -> Self {
        Self { screen_height: f64::NAN }
    }
}

impl CoordinateConverter {
    pub fn from_height(height: f64) -> Self {
        Self { screen_height: height }
    }

    pub fn screen_height(&self) -> Option<f64> {
        if self.screen_height.is_nan() {
            None
        } else {
            Some(self.screen_height)
        }
    }

    pub fn convert_point(&self, point: CGPoint) -> Option<CGPoint> {
        if self.screen_height.is_nan() {
            return None;
        }
        Some(CGPoint::new(point.x, self.screen_height - point.y))
    }

    pub fn convert_rect(&self, rect: CGRect) -> Option<CGRect> {
        if self.screen_height.is_nan() {
            return None;
        }
        Some(CGRect::new(
            CGPoint::new(rect.origin.x, self.screen_height - rect.max().y),
            rect.size,
        ))
    }
}

pub fn order_visible_spaces_by_position(
    spaces: impl IntoIterator<Item = (SpaceId, CGPoint)>,
) -> Vec<SpaceId> {
    let mut spaces: Vec<_> = spaces.into_iter().collect();

    // order spaces by the physical screen coordinates (left-to-right, then bottom-to-top).
    spaces.sort_by(|(_, a_center), (_, b_center)| {
        let x_order = a_center.x.total_cmp(&b_center.x);
        if x_order == Ordering::Equal {
            a_center.y.total_cmp(&b_center.y)
        } else {
            x_order
        }
    });

    spaces.into_iter().map(|(space, _)| space).collect()
}

#[cfg(test)]
mod tests {
    use objc2_core_foundation::CGSize;

    use super::*;

    fn rect(x: f64, y: f64, w: f64, h: f64) -> CGRect {
        CGRect::new(CGPoint::new(x, y), CGSize::new(w, h))
    }

    #[test]
    fn auto_hidden_menu_bar_does_not_inset_a_display_without_a_notch() {
        assert_eq!(super::menu_bar_inset(true, 24.0, 0.0), 0.0);
    }

    #[test]
    fn auto_hidden_menu_bar_preserves_the_notch_safe_area() {
        assert_eq!(super::menu_bar_inset(true, 37.0, 32.0), 32.0);
    }

    #[test]
    fn visible_menu_bar_reserves_its_reported_height() {
        assert_eq!(super::menu_bar_inset(false, 24.0, 0.0), 25.0);
    }

    #[test]
    fn orders_spaces_by_horizontal_position() {
        let spaces = vec![
            (SpaceId::new(1), CGPoint::new(-500.0, 0.0)),
            (SpaceId::new(2), CGPoint::new(0.0, 0.0)),
            (SpaceId::new(3), CGPoint::new(500.0, 100.0)),
        ];

        let ordered = order_visible_spaces_by_position(spaces);
        assert_eq!(ordered, vec![SpaceId::new(1), SpaceId::new(2), SpaceId::new(3)]);
    }

    #[test]
    fn orders_spaces_by_vertical_position_when_aligned() {
        let spaces = vec![
            (SpaceId::new(10), CGPoint::new(0.0, -200.0)),
            (SpaceId::new(11), CGPoint::new(0.0, 150.0)),
        ];

        let ordered = order_visible_spaces_by_position(spaces);
        assert_eq!(ordered, vec![SpaceId::new(10), SpaceId::new(11)]);
    }

    #[test]
    fn coordinate_converter_flips_y_against_the_first_screen_and_is_an_involution() {
        let converter = CoordinateConverter::from_height(1000.0);
        let point = converter.convert_point(CGPoint::new(10.0, 100.0)).unwrap();
        assert_eq!((point.x, point.y), (10.0, 900.0));
        let rect = CGRect::new(CGPoint::new(10.0, 100.0), CGSize::new(50.0, 20.0));
        let flipped = converter.convert_rect(rect).unwrap();
        assert_eq!((flipped.origin.x, flipped.origin.y), (10.0, 880.0));
        assert_eq!(flipped.size.width, 50.0);
        let back = converter.convert_rect(flipped).unwrap();
        assert_eq!((back.origin.x, back.origin.y), (10.0, 100.0));
    }

    #[test]
    fn a_default_converter_has_no_screen_and_converts_nothing() {
        let converter = CoordinateConverter::default();
        assert_eq!(converter.screen_height(), None);
        assert!(converter.convert_point(CGPoint::new(1.0, 1.0)).is_none());
        assert!(
            converter
                .convert_rect(CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(1.0, 1.0)))
                .is_none()
        );
    }

    #[test]
    fn rects_touching_at_an_edge_do_not_intersect() {
        let a = CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(10.0, 10.0));
        let b = CGRect::new(CGPoint::new(10.0, 0.0), CGSize::new(10.0, 10.0));
        let c = CGRect::new(CGPoint::new(9.0, 9.0), CGSize::new(10.0, 10.0));
        assert!(!rects_intersect(&a, &b));
        assert!(rects_intersect(&a, &c));
    }

    #[test]
    fn a_dock_the_menu_bar_already_covers_takes_nothing_more() {
        let raw = rect(0.0, 0.0, 1000.0, 800.0);
        let dock_under_the_bar = rect(0.0, 0.0, 1000.0, 20.0);
        assert_eq!(
            usable_frame(raw, 30.0, Some((dock_under_the_bar, DockEdge::Bottom))),
            usable_frame(raw, 30.0, None),
            "the overlap test is against what the bar left, not against the raw frame"
        );
    }

    // The 40 lines this replaced read the live Dock, so none of it was ever tested.
    #[test]
    fn the_menu_bar_takes_off_the_top_and_leaves_the_width_alone() {
        let usable = usable_frame(rect(0.0, 0.0, 1728.0, 1117.0), 37.0, None);
        assert_eq!(usable, rect(0.0, 37.0, 1728.0, 1080.0));
    }

    #[test]
    fn a_dock_takes_off_the_edge_it_sits_on() {
        let raw = rect(0.0, 0.0, 1000.0, 800.0);
        let dock = rect(0.0, 700.0, 1000.0, 70.0);
        assert_eq!(
            usable_frame(raw, 0.0, Some((dock, DockEdge::Bottom))),
            rect(0.0, 0.0, 1000.0, 730.0)
        );
        let side = rect(0.0, 0.0, 60.0, 800.0);
        assert_eq!(
            usable_frame(raw, 0.0, Some((side, DockEdge::Left))),
            rect(60.0, 0.0, 940.0, 800.0),
            "a left dock moves the origin as well as shrinking the width"
        );
        assert_eq!(
            usable_frame(raw, 0.0, Some((side, DockEdge::Right))),
            rect(0.0, 0.0, 940.0, 800.0),
            "a right dock only shrinks"
        );
    }

    #[test]
    fn an_unrecognised_orientation_falls_back_to_the_docks_own_shape() {
        let raw = rect(0.0, 0.0, 1000.0, 800.0);
        let wide = rect(0.0, 700.0, 1000.0, 70.0);
        assert_eq!(
            usable_frame(raw, 0.0, Some((wide, DockEdge::Unknown))),
            usable_frame(raw, 0.0, Some((wide, DockEdge::Bottom)))
        );
        let tall = rect(0.0, 0.0, 60.0, 800.0);
        assert_eq!(
            usable_frame(raw, 0.0, Some((tall, DockEdge::Unknown))),
            usable_frame(raw, 0.0, Some((tall, DockEdge::Left)))
        );
    }

    #[test]
    fn a_dock_larger_than_the_display_leaves_nothing_rather_than_a_negative_frame() {
        let raw = rect(0.0, 0.0, 100.0, 100.0);
        let huge = rect(0.0, 0.0, 400.0, 400.0);
        let usable = usable_frame(raw, 0.0, Some((huge, DockEdge::Bottom)));
        assert_eq!(usable.size.height, 0.0);
        let inset = usable_frame(raw, 500.0, None);
        assert_eq!(inset.size.height, 0.0, "a menu bar taller than the display, too");
    }
}
