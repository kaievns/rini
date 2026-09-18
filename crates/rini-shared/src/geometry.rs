//! converting between different binding's geometry types

use objc2_core_foundation as ic;
use serde::{Deserialize, Deserializer, Serialize};
use serde_with::{DeserializeAs, SerializeAs};

pub trait Round {
    fn round(&self) -> Self;
}

impl Round for ic::CGRect {
    fn round(&self) -> Self {
        let min_rounded = self.min().round();
        let max_rounded = self.max().round();
        ic::CGRect {
            origin: min_rounded,
            size: ic::CGSize {
                width: max_rounded.x - min_rounded.x,
                height: max_rounded.y - min_rounded.y,
            },
        }
    }
}

impl Round for ic::CGPoint {
    fn round(&self) -> Self {
        ic::CGPoint {
            x: self.x.round(),
            y: self.y.round(),
        }
    }
}

impl Round for ic::CGSize {
    fn round(&self) -> Self {
        ic::CGSize {
            width: self.width.round(),
            height: self.height.round(),
        }
    }
}

pub trait IsWithin {
    fn is_within(&self, how_much: f64, other: Self) -> bool;
}

impl IsWithin for ic::CGRect {
    fn is_within(&self, how_much: f64, other: Self) -> bool {
        self.origin.is_within(how_much, other.origin) && self.size.is_within(how_much, other.size)
    }
}

impl IsWithin for ic::CGPoint {
    fn is_within(&self, how_much: f64, other: Self) -> bool {
        self.x.is_within(how_much, other.x) && self.y.is_within(how_much, other.y)
    }
}

impl IsWithin for ic::CGSize {
    fn is_within(&self, how_much: f64, other: Self) -> bool {
        self.width.is_within(how_much, other.width) && self.height.is_within(how_much, other.height)
    }
}

impl IsWithin for f64 {
    fn is_within(&self, how_much: f64, other: Self) -> bool {
        (self - other).abs() < how_much
    }
}

pub trait SameAs: IsWithin + Sized {
    fn same_as(&self, other: Self) -> bool {
        self.is_within(0.1, other)
    }
}

impl SameAs for ic::CGRect {}
impl SameAs for ic::CGPoint {}
impl SameAs for ic::CGSize {}

pub trait CGRectExt {
    fn intersection(&self, other: &Self) -> Self;
    fn contains(&self, point: ic::CGPoint) -> bool;
    fn contains_rect(&self, other: Self) -> bool;
    fn area(&self) -> f64;
}

impl CGRectExt for ic::CGRect {
    fn intersection(&self, other: &Self) -> Self {
        let min_x = f64::max(self.min().x, other.min().x);
        let max_x = f64::min(self.max().x, other.max().x);
        let min_y = f64::max(self.min().y, other.min().y);
        let max_y = f64::min(self.max().y, other.max().y);
        ic::CGRect {
            origin: ic::CGPoint::new(min_x, min_y),
            size: ic::CGSize::new(f64::max(max_x - min_x, 0.), f64::max(max_y - min_y, 0.)),
        }
    }

    fn contains(&self, point: ic::CGPoint) -> bool {
        (self.min().x..=self.max().x).contains(&point.x)
            && (self.min().y..=self.max().y).contains(&point.y)
    }

    fn contains_rect(&self, other: Self) -> bool {
        self.min().x <= other.min().x
            && self.min().y <= other.min().y
            && self.max().x >= other.max().x
            && self.max().y >= other.max().y
    }

    fn area(&self) -> f64 {
        self.size.width * self.size.height
    }
}

#[derive(Serialize, Deserialize)]
#[serde(remote = "ic::CGRect")]
pub struct CGRectDef {
    #[serde(with = "CGPointDef")]
    pub origin: ic::CGPoint,
    #[serde(with = "CGSizeDef")]
    pub size: ic::CGSize,
}

#[derive(Serialize, Deserialize)]
#[serde(remote = "ic::CGPoint")]
pub struct CGPointDef {
    pub x: f64,
    pub y: f64,
}

#[derive(Serialize, Deserialize)]
#[serde(remote = "ic::CGSize")]
pub struct CGSizeDef {
    pub width: f64,
    pub height: f64,
}

impl SerializeAs<ic::CGRect> for CGRectDef {
    fn serialize_as<S>(value: &ic::CGRect, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        CGRectDef::serialize(value, serializer)
    }
}

impl<'de> DeserializeAs<'de, ic::CGRect> for CGRectDef {
    fn deserialize_as<D>(deserializer: D) -> Result<ic::CGRect, D::Error>
    where
        D: Deserializer<'de>,
    {
        CGRectDef::deserialize(deserializer)
    }
}

/// A frame keeps only a sliver within this many points on screen when macOS refuses the requested
/// off-screen position. Apps clamp further (Kiro 41pt, Finder 52pt); see
/// `crates/rini-layout/docs/strip.md` "Parking".
pub const PARK_CLAMP_PX: f64 = 40.0;

/// Whether `window` shows nothing usable on `display`: no intersection, or a sliver within
/// `PARK_CLAMP_PX` in both axes. A column peeking in at an edge shows its full height, so it is
/// never "off screen" by this test.
pub fn is_off_screen(display: ic::CGRect, window: ic::CGRect) -> bool {
    let visible_width =
        (window.max().x.min(display.max().x) - window.origin.x.max(display.origin.x)).max(0.0);
    let visible_height =
        (window.max().y.min(display.max().y) - window.origin.y.max(display.origin.y)).max(0.0);
    if visible_width <= 0.0 || visible_height <= 0.0 {
        return true;
    }
    visible_width <= PARK_CLAMP_PX && visible_height <= PARK_CLAMP_PX
}

/// Where a window parked at `park` should start an animation towards `destination`: the same row,
/// just past the display edge on the park's side, so it enters from the side it left by.
pub fn park_entry_frame(park: ic::CGRect, destination: ic::CGRect, display: ic::CGRect) -> ic::CGRect {
    let from_the_left = park.mid().x < display.mid().x;
    let x = if from_the_left {
        display.origin.x - destination.size.width
    } else {
        display.max().x
    };
    ic::CGRect::new(ic::CGPoint::new(x, destination.origin.y), destination.size)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: f64, y: f64, w: f64, h: f64) -> ic::CGRect {
        ic::CGRect::new(ic::CGPoint::new(x, y), ic::CGSize::new(w, h))
    }

    #[test]
    fn round_snaps_both_edges_so_the_size_absorbs_the_difference() {
        let r = rect(0.4, 0.6, 10.2, 10.2).round();
        assert_eq!((r.origin.x, r.origin.y), (0.0, 1.0));
        assert_eq!((r.size.width, r.size.height), (11.0, 10.0));
    }

    #[test]
    fn same_as_tolerates_a_tenth_of_a_point_and_not_more() {
        assert!(rect(0.0, 0.0, 10.0, 10.0).same_as(rect(0.09, 0.0, 10.0, 10.0)));
        assert!(!rect(0.0, 0.0, 10.0, 10.0).same_as(rect(0.1, 0.0, 10.0, 10.0)));
        assert!(!rect(0.0, 0.0, 10.0, 10.0).same_as(rect(0.0, 0.0, 10.0, 10.2)));
    }

    #[test]
    fn intersection_of_disjoint_rects_is_empty_not_negative() {
        let a = rect(0.0, 0.0, 10.0, 10.0);
        let b = rect(20.0, 20.0, 5.0, 5.0);
        let i = a.intersection(&b);
        assert_eq!((i.size.width, i.size.height), (0.0, 0.0));
        assert_eq!(a.intersection(&rect(5.0, 5.0, 10.0, 10.0)).area(), 25.0);
    }

    #[test]
    fn containment_includes_the_edges() {
        let r = rect(0.0, 0.0, 10.0, 10.0);
        assert!(r.contains(ic::CGPoint::new(10.0, 10.0)));
        assert!(!r.contains(ic::CGPoint::new(10.1, 10.0)));
        assert!(r.contains_rect(r));
        assert!(!r.contains_rect(rect(0.0, 0.0, 10.0, 10.1)));
    }

    #[test]
    fn cgrect_def_round_trips_through_serde() {
        #[derive(Serialize, Deserialize, PartialEq, Debug)]
        struct Wrapper(#[serde(with = "CGRectDef")] ic::CGRect);
        let w = Wrapper(rect(1.5, 2.5, 3.5, 4.5));
        let json = serde_json::to_string(&w).unwrap();
        assert_eq!(serde_json::from_str::<Wrapper>(&json).unwrap(), w);
    }

    #[test]
    fn a_park_sliver_is_off_screen_but_a_peeking_column_is_not() {
        let display = rect(0.0, 0.0, 1728.0, 1117.0);
        assert!(is_off_screen(display, rect(1727.0, 1116.0, 800.0, 600.0)), "1pt corner park");
        assert!(is_off_screen(display, rect(-1719.0, 1116.0, 1720.0, 600.0)), "1pt corner park, left");
        assert!(is_off_screen(display, rect(-3000.0, 0.0, 800.0, 600.0)), "no intersection");
        assert!(!is_off_screen(display, rect(1600.0, 0.0, 800.0, 1117.0)), "column peeking in 128pt");
        assert!(!is_off_screen(display, rect(-770.0, 0.0, 800.0, 1117.0)), "30pt wide but full height");
    }

    #[test]
    fn a_parked_window_enters_from_the_side_it_was_parked_on() {
        let display = rect(0.0, 0.0, 1728.0, 1117.0);
        let destination = rect(400.0, 32.0, 800.0, 1000.0);
        let from_left = park_entry_frame(rect(-799.0, 1116.0, 800.0, 600.0), destination, display);
        assert_eq!((from_left.origin.x, from_left.origin.y), (-800.0, 32.0));
        let from_right = park_entry_frame(rect(1727.0, 1116.0, 800.0, 600.0), destination, display);
        assert_eq!(from_right.origin.x, 1728.0);
        assert_eq!(from_right.size.width, 800.0);
    }
}
