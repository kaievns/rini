//! Handing a picture to a Core Animation layer. One place decides how pixels become layer contents,
//! for the flight overlay, the switcher's cards and the bar alike.

use objc2_core_foundation::CFType;
use objc2_quartz_core::CALayer;
use rini_geometry::space::{ContentsRect, Point, Size, ToCg};

use crate::pictures::domain::region::WHOLE;
use crate::pictures::platform::picture::{Picture, Pixels, Route};

/// Hands a captured picture to a layer, whichever kind of pixels it holds: a region as its whole
/// composite with `contentsRect` on its part, anything else whole. Written for every kind, because
/// layers are pooled.
pub fn set_layer_contents(layer: &CALayer, picture: &Picture) {
    match &picture.image {
        Pixels::Bitmap(image) => set_contents(layer, image),
        Pixels::Surface(surface) => set_contents(layer, surface),
        Pixels::Region { composite, .. } => set_contents(layer, composite),
    }
    layer.setContentsRect(picture.image.unit_rect().to_cg());
}

/// Hands `picture`, a `CGImage` or an `IOSurface`, to `layer` as its contents.
///
/// As an object: a `CGImage` or `IOSurfaceRef` pointer handed to `msg_send!` encodes as a struct
/// pointer, which objc2 refuses for `setContents:` in any build with debug assertions.
pub fn set_contents(layer: &CALayer, picture: &CFType) {
    // SAFETY: Core Animation draws a CGImage or an IOSurface as a layer's contents, and retains it.
    unsafe { layer.setContents(Some(picture.as_ref())) }
}

/// The part of a layer's contents that stretches, for `contentsCenter`. All of it for a picture; for
/// the stand-in only its middle, so the rounded corners keep the window's radius at any size.
pub fn stretch_region(picture: &Picture) -> ContentsRect {
    let Pixels::Bitmap(image) = &picture.image else {
        return WHOLE;
    };
    if picture.source != Route::StandIn {
        return WHOLE;
    }
    let side = objc2_core_graphics::CGImage::width(Some(image)) as f64;
    if side < 3.0 {
        return WHOLE;
    }
    // The stand-in is a square of two corners and a two-pixel middle; see `stand_in_image`.
    let corner = (side - 2.0) / 2.0;
    ContentsRect::new(
        Point::new(corner / side, corner / side),
        Size::new(2.0 / side, 2.0 / side),
    )
}

#[cfg(test)]
mod tests {
    use objc2::rc::Retained;
    use objc2_core_foundation::{CFRetained, CGPoint, CGRect, CGSize};
    use rini_geometry::space::{FromCg, PixelRect, ScreenSize};

    use super::*;
    use crate::pictures::platform::bitmap::rgba_bitmap_context;
    use crate::pictures::platform::picture::{stand_in_picture, stand_in_image, test_picture};

    fn test_surface() -> CFRetained<objc2_io_surface::IOSurfaceRef> {
        use objc2_core_foundation::{CFDictionary, CFNumber, CFString};
        use objc2_io_surface::{
            IOSurfaceRef, kIOSurfaceBytesPerElement, kIOSurfaceHeight, kIOSurfaceWidth,
        };
        let keys: [&CFString; 3] =
            unsafe { [kIOSurfaceWidth, kIOSurfaceHeight, kIOSurfaceBytesPerElement] };
        let values = [
            CFNumber::new_i64(1),
            CFNumber::new_i64(1),
            CFNumber::new_i64(4),
        ];
        let value_refs: [&CFNumber; 3] = std::array::from_fn(|i| &*values[i]);
        // SAFETY: the three keys an IOSurface needs, each with a number.
        unsafe { IOSurfaceRef::new(CFDictionary::from_slices(&keys, &value_refs).as_opaque()) }
            .expect("a surface")
    }

    fn contents_address(layer: &CALayer) -> Option<usize> {
        // SAFETY: a read of the layer's own property.
        unsafe { layer.contents() }.map(|object| Retained::as_ptr(&object) as usize)
    }

    /// Either kind of picture reaches the layer, under the debug assertions every test build has.
    /// Handed to `setContents:` as a raw `CGImage` or `IOSurfaceRef` pointer instead, the message
    /// fails objc2's encoding check and panics, in a debug build on the first bar, flight or switch.
    #[test]
    fn a_layer_takes_a_picture_of_either_kind() {
        let layer = CALayer::layer();

        let bitmap = test_picture(ScreenSize::new(1.0, 1.0));
        set_layer_contents(&layer, &bitmap);
        let Pixels::Bitmap(image) = &bitmap.image else {
            unreachable!("a test picture is a bitmap")
        };
        assert_eq!(contents_address(&layer), Some(&**image as *const _ as usize));

        let surface = test_surface();
        let on_surface = Picture {
            image: Pixels::Surface(surface.clone()),
            ..test_picture(ScreenSize::new(1.0, 1.0))
        };
        set_layer_contents(&layer, &on_surface);
        assert_eq!(contents_address(&layer), Some(&*surface as *const _ as usize));
    }

    /// A composite of four flat quadrants, 16x8 pixels: red, green across the top, blue, white below.
    fn quadrants() -> CFRetained<objc2_core_graphics::CGImage> {
        use objc2_core_graphics::CGContext;
        let ctx = rgba_bitmap_context(16, 8).expect("a context");
        // User space has its origin at the bottom left: the image's top rows are y 4..8.
        for (x, y, (r, g, b)) in [
            (0.0, 4.0, (1.0, 0.0, 0.0)),
            (8.0, 4.0, (0.0, 1.0, 0.0)),
            (0.0, 0.0, (0.0, 0.0, 1.0)),
            (8.0, 0.0, (1.0, 1.0, 1.0)),
        ] {
            CGContext::set_rgb_fill_color(Some(&ctx), r, g, b, 1.0);
            CGContext::fill_rect(Some(&ctx), CGRect::new(CGPoint::new(x, y), CGSize::new(8.0, 4.0)));
        }
        objc2_core_graphics::CGBitmapContextCreateImage(Some(&ctx)).expect("an image")
    }

    /// Every pixel of `width` x `height`, RGBA, top row first, of whatever `draw` puts in a fresh
    /// context.
    fn drawn(width: usize, height: usize, draw: impl Fn(&objc2_core_graphics::CGContext)) -> Vec<u8> {
        let ctx = rgba_bitmap_context(width, height).expect("a context");
        draw(&ctx);
        let data = objc2_core_graphics::CGBitmapContextGetData(Some(&ctx)) as *const u8;
        let stride = objc2_core_graphics::CGBitmapContextGetBytesPerRow(Some(&ctx));
        (0..height)
            .flat_map(|y| {
                // SAFETY: rows within the context's own buffer.
                unsafe { std::slice::from_raw_parts(data.add(y * stride), width * 4) }.to_vec()
            })
            .collect()
    }

    /// A region drawn by a layer in a flipped tree is its crop, pixel for pixel: there a layer's
    /// `contentsRect` counts down from the image's top, as a region's rect does. Every client draws in
    /// one, under a flipped `NSView`. In a tree that is not flipped it counts up from the bottom, and
    /// the same region would show the quadrant below.
    #[test]
    fn a_layer_shows_a_region_as_its_crop() {
        use objc2_quartz_core::{kCAFilterNearest, kCAGravityResize};
        let composite = quadrants();
        let render = |flipped: bool, layer: &CALayer| {
            let tree = CALayer::layer();
            tree.setBounds(CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(8.0, 4.0)));
            tree.setAnchorPoint(CGPoint::new(0.0, 0.0));
            tree.setGeometryFlipped(flipped);
            layer.setAnchorPoint(CGPoint::new(0.0, 0.0));
            tree.addSublayer(layer);
            // A flipped tree is shown top row first; a bitmap context counts up from the bottom.
            let shown = drawn(8, 4, |ctx| {
                if flipped {
                    objc2_core_graphics::CGContext::translate_ctm(Some(ctx), 0.0, 4.0);
                    objc2_core_graphics::CGContext::scale_ctm(Some(ctx), 1.0, -1.0);
                }
                tree.renderInContext(ctx)
            });
            layer.removeFromSuperlayer();
            shown
        };
        for (rect, colour) in [
            (CGRect::new(CGPoint::new(8.0, 0.0), CGSize::new(8.0, 4.0)), [0, 255, 0, 255]),
            (CGRect::new(CGPoint::new(0.0, 4.0), CGSize::new(8.0, 4.0)), [0, 0, 255, 255]),
            (CGRect::new(CGPoint::new(4.0, 2.0), CGSize::new(8.0, 4.0)), [255, 0, 0, 255]),
        ] {
            let region = Picture {
                image: Pixels::Region {
                    composite: composite.clone(),
                    rect: PixelRect::from_cg(rect),
                },
                ..test_picture(ScreenSize::new(8.0, 4.0))
            };
            let layer = CALayer::layer();
            layer.setBounds(CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(8.0, 4.0)));
            layer.setContentsScale(1.0);
            // SAFETY: Core Animation's own constants.
            unsafe {
                layer.setContentsGravity(kCAGravityResize);
                layer.setMagnificationFilter(kCAFilterNearest);
                layer.setMinificationFilter(kCAFilterNearest);
            }
            set_layer_contents(&layer, &region);
            let shown = render(true, &layer);
            let crop = region.image.bitmap().expect("a crop");
            let cropped = drawn(8, 4, |ctx| {
                objc2_core_graphics::CGContext::draw_image(
                    Some(ctx),
                    CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(8.0, 4.0)),
                    Some(&crop),
                )
            });
            assert_eq!(shown, cropped, "{rect:?}");
            assert_eq!(&shown[..4], &colour, "{rect:?}: the region's top-left pixel");
        }

        let top_right = Picture {
            image: Pixels::Region {
                composite,
                rect: PixelRect::new(Point::new(8.0, 0.0), Size::new(8.0, 4.0)),
            },
            ..test_picture(ScreenSize::new(8.0, 4.0))
        };
        let layer = CALayer::layer();
        layer.setBounds(CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(8.0, 4.0)));
        layer.setContentsScale(1.0);
        set_layer_contents(&layer, &top_right);
        assert_eq!(&render(false, &layer)[..4], &[255, 255, 255, 255], "unflipped: the quadrant below");
    }

    /// A layer that last showed a region shows the next picture whole: layers are pooled.
    #[test]
    fn a_picture_after_a_region_is_shown_whole() {
        let layer = CALayer::layer();
        let region = Picture {
            image: Pixels::Region {
                composite: quadrants(),
                rect: PixelRect::new(Point::new(8.0, 0.0), Size::new(8.0, 4.0)),
            },
            ..test_picture(ScreenSize::new(8.0, 4.0))
        };
        set_layer_contents(&layer, &region);
        assert_eq!(
            layer.contentsRect(),
            CGRect::new(CGPoint::new(0.5, 0.0), CGSize::new(0.5, 0.5))
        );
        set_layer_contents(&layer, &test_picture(ScreenSize::new(1.0, 1.0)));
        assert_eq!(layer.contentsRect(), WHOLE.to_cg());
    }

    /// A stand-in stretches only its two-pixel middle, so its corners keep the window's radius; a real
    /// picture stretches whole. Written every install, since tile layers are pooled.
    #[test]
    fn only_a_stand_in_stretches_nine_slice() {
        let (image, _) = stand_in_image(2.0).expect("drawn");
        let stand_in = stand_in_picture(ScreenSize::new(859.0, 1081.0), image);

        let region = stretch_region(&stand_in);
        assert!((region.origin.x - 20.0 / 42.0).abs() < 1e-9);
        assert!((region.size.width - 2.0 / 42.0).abs() < 1e-9);

        let whole = stretch_region(&test_picture(ScreenSize::new(859.0, 1081.0)));
        assert_eq!(whole, WHOLE);
    }
}
