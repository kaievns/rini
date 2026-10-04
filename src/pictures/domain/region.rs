//! Where each window lies in a batched SkyLight composite, in pixels and in unit space: the arithmetic
//! of a picture that is a region of a larger image. Plain rects, so every rule is tested on numbers.
//!
//! One `SLSHWCaptureWindowList` call for several windows returns one composite of them, the union of
//! what each has on its display. See "SLSHWCaptureWindowList captures only the visible portion" in
//! `src/animation/docs/capture-overlay-research.md`.

use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use rini_geometry::CGRectExt;

/// How far a composite's two scales may differ, and how far either may be from a whole number, before
/// the composite is not the shape of the windows it was asked for: a listed frame is wrong, and every
/// crop with it.
const SCALE_SLACK: f64 = 0.02;

/// How far a crop may overrun its composite, in pixels, from rounding alone.
const CONTAINMENT_SLACK: f64 = 0.5;

/// `frame`, a rect in global points, as a pixel rect in a composite whose top-left is `origin` at
/// `scale`, rounded to whole pixels.
pub fn pixel_rect(frame: CGRect, origin: CGPoint, scale: f64) -> CGRect {
    CGRect::new(
        CGPoint::new(
            ((frame.origin.x - origin.x) * scale).round(),
            ((frame.origin.y - origin.y) * scale).round(),
        ),
        CGSize::new((frame.size.width * scale).round(), (frame.size.height * scale).round()),
    )
}

/// Whether `rect` lies inside a composite of `size` pixels. One that does not is a window that was
/// not where it was thought to be: cropped, it gives part of another window, or nothing.
pub fn contains(size: (f64, f64), rect: CGRect) -> bool {
    rect.origin.x >= 0.0
        && rect.origin.y >= 0.0
        && rect.origin.x + rect.size.width <= size.0 + CONTAINMENT_SLACK
        && rect.origin.y + rect.size.height <= size.1 + CONTAINMENT_SLACK
}

/// The rect a composite of the windows at `frames` covers on `display`, in global points: the union of
/// what each has inside the display. `None` when none of them is on it.
pub fn composite_frame(frames: &[CGRect], display: CGRect) -> Option<CGRect> {
    frames
        .iter()
        .map(|frame| frame.intersection(&display))
        .filter(|on| on.area() > 0.0)
        .reduce(|union, frame| union.union(&frame))
}

/// The scale of a composite of `size` pixels that covers `frame`, if the composite is that shape: the
/// same scale across as down, and a whole one. Otherwise no picture of the call is drawn.
pub fn composite_scale(frame: CGRect, size: (f64, f64)) -> Option<f64> {
    if frame.size.width <= 0.0 || frame.size.height <= 0.0 {
        return None;
    }
    let across = size.0 / frame.size.width;
    let down = size.1 / frame.size.height;
    let shaped = (across - down).abs() <= SCALE_SLACK && (across - across.round()).abs() <= SCALE_SLACK;
    (shaped && across.round() >= 1.0).then_some(across)
}

/// `rect`, a pixel rect of an image of `size` pixels, in the image's unit space: what a layer's
/// `contentsRect` is given to draw that part alone. Both count down from the image's top, which holds
/// for a layer in a flipped tree: the overlay, the switcher and the bar each draw under a flipped
/// `NSView` (`a_layer_shows_a_region_as_its_crop` in `platform/layer.rs`).
pub fn unit_rect(rect: CGRect, size: (f64, f64)) -> CGRect {
    CGRect::new(
        CGPoint::new(rect.origin.x / size.0, rect.origin.y / size.1),
        CGSize::new(rect.size.width / size.0, rect.size.height / size.1),
    )
}

/// `inner`, a rect in a picture's unit space, in the unit space of the image `outer` cuts that picture
/// from: a crop piece of a tile whose picture is a region.
pub fn within(outer: CGRect, inner: CGRect) -> CGRect {
    CGRect::new(
        CGPoint::new(
            outer.origin.x + inner.origin.x * outer.size.width,
            outer.origin.y + inner.origin.y * outer.size.height,
        ),
        CGSize::new(inner.size.width * outer.size.width, inner.size.height * outer.size.height),
    )
}

/// The whole of an image, in unit space.
pub const WHOLE: CGRect = CGRect {
    origin: CGPoint { x: 0.0, y: 0.0 },
    size: CGSize {
        width: 1.0,
        height: 1.0,
    },
};

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: f64, y: f64, w: f64, h: f64) -> CGRect {
        CGRect::new(CGPoint::new(x, y), CGSize::new(w, h))
    }

    const LAPTOP: CGRect = CGRect {
        origin: CGPoint { x: 0.0, y: 0.0 },
        size: CGSize {
            width: 1728.0,
            height: 1117.0,
        },
    };

    #[test]
    fn a_windows_pixel_rect_is_its_frame_from_the_composites_corner_at_its_scale_rounded() {
        let at = pixel_rect(rect(864.25, 32.0, 859.5, 1081.0), CGPoint::new(4.0, 32.0), 2.0);
        assert_eq!(at, rect(1721.0, 0.0, 1719.0, 2162.0));
    }

    #[test]
    fn a_crop_must_lie_inside_its_composite_but_for_half_a_pixel() {
        let size = (3440.0, 2162.0);
        assert!(contains(size, rect(0.0, 0.0, 3440.0, 2162.0)));
        assert!(contains(size, rect(1721.0, 0.0, 1719.5, 2162.5)), "rounding");
        assert!(!contains(size, rect(1721.0, 0.0, 1720.0, 2162.0)), "a pixel past the edge");
        assert!(!contains(size, rect(-1.0, 0.0, 100.0, 100.0)), "before the corner");
    }

    /// The composite of windows wholly on the display is their union; of one past its edge, what the
    /// display holds of it, which is all SkyLight returns.
    #[test]
    fn a_composite_covers_the_union_of_what_each_window_has_on_the_display() {
        let strip = [rect(4.0, 32.0, 859.0, 1081.0), rect(865.0, 32.0, 859.0, 1081.0)];
        assert_eq!(composite_frame(&strip, LAPTOP), Some(rect(4.0, 32.0, 1720.0, 1081.0)));
        let past_the_edge = [rect(1300.0, 32.0, 859.0, 1081.0)];
        assert_eq!(composite_frame(&past_the_edge, LAPTOP), Some(rect(1300.0, 32.0, 428.0, 1081.0)));
        assert_eq!(composite_frame(&[rect(2000.0, 0.0, 10.0, 10.0)], LAPTOP), None);
        assert_eq!(composite_frame(&[], LAPTOP), None);
    }

    /// A composite whose shape is not the windows' says a listed frame is wrong: a window moved
    /// between the read and the call, or one is missing from it.
    #[test]
    fn a_composite_of_the_wrong_shape_has_no_scale() {
        let union = rect(4.0, 32.0, 1720.0, 1081.0);
        assert_eq!(composite_scale(union, (3440.0, 2162.0)), Some(2.0));
        assert_eq!(composite_scale(union, (1720.0, 1081.0)), Some(1.0), "a 1x display");
        assert_eq!(composite_scale(union, (3440.0, 2140.0)), None, "shorter than wide");
        assert_eq!(composite_scale(union, (2580.0, 1621.5)), None, "a scale of 1.5");
        assert_eq!(composite_scale(union, (3480.0, 2187.0)), None, "2.02 both ways: no whole scale");
        assert_eq!(composite_scale(union, (3460.0, 2175.0)), Some(3460.0 / 1720.0), "2.01: rounding");
        assert_eq!(composite_scale(rect(0.0, 0.0, 0.0, 10.0), (10.0, 10.0)), None);
    }

    /// The baked call lists the desktop after the windows, and its wallpaper spans the display: the
    /// composite is the display. Without the wallpaper it is smaller, and the same arithmetic says so.
    #[test]
    fn a_baked_composite_with_the_wallpaper_is_the_display() {
        let window = rect(4.0, 32.0, 859.0, 1081.0);
        let union = composite_frame(&[window, LAPTOP], LAPTOP).unwrap();
        assert_eq!(union, LAPTOP);
        assert_eq!(composite_scale(union, (3456.0, 2234.0)), Some(2.0));
        assert_eq!(composite_scale(union, (1718.0, 2162.0)), None, "the window alone came back");
    }

    #[test]
    fn a_unit_rect_is_the_pixel_rect_over_the_image() {
        let unit = unit_rect(rect(1720.0, 0.0, 1720.0, 2162.0), (3440.0, 2162.0));
        assert_eq!(unit, rect(0.5, 0.0, 0.5, 1.0));
        assert_eq!(unit_rect(rect(0.0, 0.0, 10.0, 10.0), (10.0, 10.0)), WHOLE);
    }

    /// A crop piece's unit rect is relative to the picture; composed with a region it lands inside
    /// the region of the composite.
    #[test]
    fn a_piece_within_a_region_lands_inside_the_region() {
        let region = rect(0.5, 0.0, 0.5, 1.0);
        assert_eq!(within(region, rect(0.0, 0.0, 0.5, 0.5)), rect(0.5, 0.0, 0.25, 0.5));
        assert_eq!(within(region, rect(0.5, 0.5, 0.5, 0.5)), rect(0.75, 0.5, 0.25, 0.5));
        assert_eq!(within(region, WHOLE), region);
        assert_eq!(within(WHOLE, rect(0.2, 0.3, 0.4, 0.5)), rect(0.2, 0.3, 0.4, 0.5));
    }
}
