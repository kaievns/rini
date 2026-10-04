//! Core Graphics bitmaps: a fresh RGBA context to draw into, an image's copy of its own, and an image's
//! thumbprint.

use objc2_core_foundation::{CFRetained, CGPoint, CGRect, CGSize};
use objc2_core_graphics::{
    CGBitmapContextGetBytesPerRow, CGBitmapContextGetData, CGColorSpace, CGContext, CGImage,
    CGImageAlphaInfo, CGImageByteOrderInfo,
};

/// A fresh `width` x `height` RGBA bitmap context, alpha last and premultiplied, owning its buffer.
pub fn rgba_bitmap_context(width: usize, height: usize) -> Option<CFRetained<CGContext>> {
    let space = CGColorSpace::new_device_rgb()?;
    // SAFETY: a fresh context; CG owns and frees the backing store with it.
    let ctx = unsafe {
        CGBitmapContextCreate(
            std::ptr::null_mut(),
            width,
            height,
            8,
            0,
            Some(&space),
            CGImageAlphaInfo::PremultipliedLast.0,
        )
    };
    unsafe { Some(CFRetained::from_raw(std::ptr::NonNull::new(ctx)?)) }
}

/// `image` drawn into an image of its own, premultiplied BGRA, the layout Core Animation draws from
/// without converting: a region kept as its own copy, so it no longer holds its composite alive.
pub fn standalone_copy(image: &CGImage) -> Option<CFRetained<CGImage>> {
    let (width, height) = (CGImage::width(Some(image)), CGImage::height(Some(image)));
    if width == 0 || height == 0 {
        return None;
    }
    let space = CGImage::color_space(Some(image)).or_else(CGColorSpace::new_device_rgb)?;
    let bgra = CGImageAlphaInfo::PremultipliedFirst.0 | CGImageByteOrderInfo::Order32Little.0;
    // SAFETY: a fresh context; CG owns and frees the backing store with it.
    let ctx = unsafe {
        CGBitmapContextCreate(std::ptr::null_mut(), width, height, 8, 0, Some(&space), bgra)
    };
    let ctx = unsafe { CFRetained::from_raw(std::ptr::NonNull::new(ctx)?) };
    CGContext::draw_image(
        Some(&ctx),
        CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(width as f64, height as f64)),
        Some(image),
    );
    objc2_core_graphics::CGBitmapContextCreateImage(Some(&ctx))
}

// Not bound by objc2-core-graphics 0.3, which only generates the adaptive variant.
unsafe extern "C-unwind" {
    fn CGBitmapContextCreate(
        data: *mut core::ffi::c_void,
        width: usize,
        height: usize,
        bits_per_component: usize,
        bytes_per_row: usize,
        space: Option<&CGColorSpace>,
        bitmap_info: u32,
    ) -> *mut CGContext;
}

/// The side of the square a [`thumbprint`] samples an image down to.
const THUMBPRINT_SIDE: usize = 32;

/// A small fingerprint of an image's content, for the reveal chase's settledness check.
pub fn thumbprint(image: &CGImage) -> Option<Vec<u8>> {
    let side = THUMBPRINT_SIDE;
    let ctx = rgba_bitmap_context(side, side)?;
    CGContext::draw_image(
        Some(&ctx),
        CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(side as f64, side as f64)),
        Some(image),
    );
    let data = CGBitmapContextGetData(Some(&ctx)) as *const u8;
    if data.is_null() {
        return None;
    }
    let stride = CGBitmapContextGetBytesPerRow(Some(&ctx));
    let mut out = Vec::with_capacity(side * side * 4);
    for y in 0..side {
        // SAFETY: y < side, rows within the context's own buffer.
        out.extend_from_slice(unsafe {
            std::slice::from_raw_parts(data.add(y * stride), side * 4)
        });
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use objc2_core_graphics::CGBitmapContextCreateImage;

    use super::*;

    fn flat_image(w: usize, h: usize, grey: f64) -> CFRetained<CGImage> {
        let ctx = rgba_bitmap_context(w, h).unwrap();
        CGContext::set_rgb_fill_color(Some(&ctx), grey, grey, grey, 1.0);
        CGContext::fill_rect(
            Some(&ctx),
            CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(w as f64, h as f64)),
        );
        CGBitmapContextCreateImage(Some(&ctx)).unwrap()
    }

    /// A copy has the pixels of what it was made of, in an image of its own: a crop's copy no longer
    /// holds the image it was cut from.
    #[test]
    fn a_copy_has_the_pixels_of_its_source_in_an_image_of_its_own() {
        let ctx = rgba_bitmap_context(4, 2).unwrap();
        CGContext::set_rgb_fill_color(Some(&ctx), 1.0, 0.0, 0.0, 1.0);
        CGContext::fill_rect(Some(&ctx), CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(2.0, 2.0)));
        CGContext::set_rgb_fill_color(Some(&ctx), 0.0, 0.0, 1.0, 1.0);
        CGContext::fill_rect(Some(&ctx), CGRect::new(CGPoint::new(2.0, 0.0), CGSize::new(2.0, 2.0)));
        let source = CGBitmapContextCreateImage(Some(&ctx)).unwrap();
        let crop = CGImage::with_image_in_rect(
            Some(&source),
            CGRect::new(CGPoint::new(2.0, 0.0), CGSize::new(2.0, 2.0)),
        )
        .unwrap();
        let copy = standalone_copy(&crop).expect("a copy");
        assert_eq!((CGImage::width(Some(&copy)), CGImage::height(Some(&copy))), (2, 2));
        assert_ne!(&*copy as *const CGImage, &*crop as *const CGImage);
        assert_eq!(thumbprint(&copy), thumbprint(&crop), "the same pixels");
        assert_eq!(thumbprint(&copy), thumbprint(&flat_blue(2, 2)));
    }

    fn flat_blue(w: usize, h: usize) -> CFRetained<CGImage> {
        let ctx = rgba_bitmap_context(w, h).unwrap();
        CGContext::set_rgb_fill_color(Some(&ctx), 0.0, 0.0, 1.0, 1.0);
        CGContext::fill_rect(
            Some(&ctx),
            CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(w as f64, h as f64)),
        );
        CGBitmapContextCreateImage(Some(&ctx)).unwrap()
    }

    /// The chase compares prints of one window at two sizes, so a print is the same size whatever
    /// the image's, and the same content prints the same.
    #[test]
    fn a_thumbprint_is_one_small_square_whatever_the_image() {
        let small = thumbprint(&flat_image(60, 40, 0.5)).expect("printed");
        let large = thumbprint(&flat_image(1720, 1081, 0.5)).expect("printed");
        assert_eq!(small.len(), THUMBPRINT_SIDE * THUMBPRINT_SIDE * 4);
        assert_eq!(small, large);
        assert_ne!(small, thumbprint(&flat_image(60, 40, 0.1)).expect("printed"));
    }
}
