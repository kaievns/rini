//! Handing a picture to a Core Animation layer. One place decides how pixels become layer contents,
//! for the flight overlay, the switcher's cards and the bar alike.

use objc2_core_foundation::{CFType, CGPoint, CGRect, CGSize};
use objc2_quartz_core::CALayer;

use crate::pictures::platform::picture::{Picture, Pixels, Route};

/// Hands a captured picture to a layer, whichever kind of pixels it holds.
pub fn set_layer_contents(layer: &CALayer, picture: &Picture) {
    match &picture.image {
        Pixels::Bitmap(image) => set_contents(layer, image),
        Pixels::Surface(surface) => set_contents(layer, surface),
    }
}

/// Hands `picture`, a `CGImage` or an `IOSurface`, to `layer` as its contents.
///
/// As an object: a `CGImage` or `IOSurfaceRef` pointer handed to `msg_send!` encodes as a struct
/// pointer, which objc2 refuses for `setContents:` in any build with debug assertions.
pub fn set_contents(layer: &CALayer, picture: &CFType) {
    // SAFETY: Core Animation draws a CGImage or an IOSurface as a layer's contents, and retains it.
    unsafe { layer.setContents(Some(picture.as_ref())) }
}

/// The part of a layer's contents that stretches, in unit coordinates, for `contentsCenter`. All of it
/// for a picture; for the stand-in only its middle, so the rounded corners keep the window's radius at
/// any size.
pub fn stretch_region(picture: &Picture) -> CGRect {
    let whole = CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(1.0, 1.0));
    let Pixels::Bitmap(image) = &picture.image else {
        return whole;
    };
    if picture.source != Route::StandIn {
        return whole;
    }
    let side = objc2_core_graphics::CGImage::width(Some(image)) as f64;
    if side < 3.0 {
        return whole;
    }
    // The stand-in is a square of two corners and a two-pixel middle; see `stand_in_image`.
    let corner = (side - 2.0) / 2.0;
    CGRect::new(
        CGPoint::new(corner / side, corner / side),
        CGSize::new(2.0 / side, 2.0 / side),
    )
}

#[cfg(test)]
mod tests {
    use objc2::rc::Retained;
    use objc2_core_foundation::CFRetained;

    use super::*;
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

        let bitmap = test_picture(CGSize::new(1.0, 1.0));
        set_layer_contents(&layer, &bitmap);
        let Pixels::Bitmap(image) = &bitmap.image else {
            unreachable!("a test picture is a bitmap")
        };
        assert_eq!(contents_address(&layer), Some(&**image as *const _ as usize));

        let surface = test_surface();
        let on_surface = Picture {
            image: Pixels::Surface(surface.clone()),
            ..test_picture(CGSize::new(1.0, 1.0))
        };
        set_layer_contents(&layer, &on_surface);
        assert_eq!(contents_address(&layer), Some(&*surface as *const _ as usize));
    }

    /// A stand-in stretches only its two-pixel middle, so its corners keep the window's radius; a real
    /// picture stretches whole. Written every install, since tile layers are pooled.
    #[test]
    fn only_a_stand_in_stretches_nine_slice() {
        let (image, _) = stand_in_image(2.0).expect("drawn");
        let stand_in = stand_in_picture(CGSize::new(859.0, 1081.0), image);

        let region = stretch_region(&stand_in);
        assert!((region.origin.x - 20.0 / 42.0).abs() < 1e-9);
        assert!((region.size.width - 2.0 / 42.0).abs() < 1e-9);

        let whole = stretch_region(&test_picture(CGSize::new(859.0, 1081.0)));
        assert_eq!(whole, CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(1.0, 1.0)));
    }
}
