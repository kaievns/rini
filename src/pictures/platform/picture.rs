//! A picture of a window, the desktop or the bar: its pixels, how much of its window they cover, and
//! the route that took them. The stand-in a window with no picture flies with, and what the cache
//! keeps of a picture across captures.
//!
//! SkyLight captures what is on screen, fresh (`skylight`); ScreenCaptureKit serves everything else in
//! the background (`screencapturekit`). Constraints and costs of both:
//! `src/pictures/docs/capture-research.md`.

use objc2_core_foundation::{CFRetained, CGPoint, CGRect, CGSize};
use objc2_core_graphics::CGImage;
use objc2_io_surface::IOSurfaceRef;
use rini_geometry::space::{
    ContentsRect, PixelRect, PixelSize, PointSpace, ScreenSize, Size, ToCg,
};

use crate::pictures::domain::cache::{CarriesOver, HasCoverage, keeps_blur};
use crate::pictures::domain::coverage::{Coverage, fits_frame};
use crate::pictures::domain::region::{WHOLE, unit_rect};
use crate::pictures::domain::route::Route;
use crate::pictures::platform::bitmap::{rgba_bitmap_context, standalone_copy};

/// A window's pixels, whichever API produced them. Core Animation accepts each form.
#[derive(Clone, Debug)]
pub enum Pixels {
    /// CPU-side bitmap, from `SLSHWCaptureWindowList`.
    Bitmap(CFRetained<CGImage>),
    /// GPU-side surface, from ScreenCaptureKit; off the process's heap.
    Surface(CFRetained<IOSurfaceRef>),
    /// One window's part of a batched SkyLight composite, `rect` in the composite's pixels. Drawn
    /// through `contentsRect`, so nothing is copied out of it; it keeps the whole composite alive, so
    /// it is replaced by a copy of its own once no transition holds the service.
    Region {
        composite: CFRetained<CGImage>,
        rect: PixelRect,
    },
}

// SAFETY: for `Surface` alone, since `CFRetained<CGImage>` is `Send` already: an IOSurface is
// shareable across threads and processes, and objc2-io-surface leaves `IOSurfaceRef` unmarked.
unsafe impl Send for Pixels {}

impl Pixels {
    /// The pixels as an image of their own: the bitmap, or a crop of a region, which decodes nothing
    /// until it is drawn. `None` for a surface.
    pub fn bitmap(&self) -> Option<CFRetained<CGImage>> {
        match self {
            Self::Bitmap(image) => Some(image.clone()),
            Self::Region { composite, rect } => {
                CGImage::with_image_in_rect(Some(composite), rect.to_cg())
            }
            Self::Surface(_) => None,
        }
    }

    /// The part of the layer contents these pixels are: a region's rect in its composite, the whole
    /// of anything else.
    pub fn unit_rect(&self) -> ContentsRect {
        match self {
            Self::Region { composite, rect } => unit_rect(*rect, image_size(composite)),
            Self::Bitmap(_) | Self::Surface(_) => WHOLE,
        }
    }
}

/// An image's size.
pub fn image_size(image: &CGImage) -> PixelSize {
    PixelSize::new(CGImage::width(Some(image)) as f64, CGImage::height(Some(image)) as f64)
}

/// A window's pixels, plus how much of the window they cover and how they were taken.
#[derive(Clone, Debug)]
pub struct Picture {
    pub image: Pixels,
    pub coverage: Coverage,
    pub source: Route,
    /// When the pixels were captured; staleness is a reason to re-warm.
    pub taken: std::time::Instant,
    /// Whether this picture carries a blur its window's own surface lacks: taken as a composite with
    /// what was below the window, and different from the window's own capture where it matters. A
    /// picture like this is not given up for a same-sized capture of the window alone, which would
    /// be flat grey exactly where the blur is. See [`crate::pictures::domain::translucency`].
    pub carries_blur: bool,
}

impl Picture {
    pub fn is_usable(&self) -> bool {
        self.coverage.is_usable()
    }

    /// This picture as one to keep: a region copied into an image of its own, so it no longer holds
    /// its composite alive. `None` for any other kind, which holds nothing but itself.
    pub fn kept(&self) -> Option<Picture> {
        let Pixels::Region { .. } = &self.image else {
            return None;
        };
        let copy = standalone_copy(&*self.image.bitmap()?)?;
        Some(Picture {
            image: Pixels::Bitmap(copy),
            ..self.clone()
        })
    }

    /// Can this picture be drawn at `size` without visibly distorting it? See [`fits_frame`].
    pub fn fits<U: PointSpace>(&self, size: Size<U>) -> bool {
        fits_frame(self.coverage.covered, size)
    }
}

/// `--n3`, the raised plane of the Okibi dark theme: the same stand-in the switcher draws for a row
/// with no picture.
const STAND_IN_RGB: (f64, f64, f64) = (0.133, 0.145, 0.153);

/// The stand-in for a window of `window` size with no picture.
///
/// Claims to cover NOTHING of the window, which is honest and is also what makes a layout pass treat
/// it as a picture waiting for its reveal: the flight holds for the window and watches it until it has
/// repainted where it ends. A strip movement, which never holds, captures it once it lands there.
/// Either way its real picture replaces this one on the same tile.
pub fn stand_in_picture(window: ScreenSize, image: CFRetained<CGImage>) -> Picture {
    Picture {
        image: Pixels::Bitmap(image),
        coverage: Coverage {
            covered: ScreenSize::zero(),
            window,
        },
        source: Route::StandIn,
        taken: std::time::Instant::now(),
        carries_blur: false,
    }
}

/// The stand-in's pixels: one pixel of `STAND_IN_RGB`, which the overlay stretches over the tile and
/// clips to its window's corner. One pixel rather than the window's size: a full-size bitmap of one
/// flat colour would be 30MB for a maximised window at 2x.
pub fn stand_in_image() -> Option<CFRetained<CGImage>> {
    use objc2_core_graphics::CGContext;
    let ctx = rgba_bitmap_context(1, 1)?;
    let (r, g, b) = STAND_IN_RGB;
    CGContext::set_rgb_fill_color(Some(&ctx), r, g, b, 1.0);
    CGContext::fill_rect(Some(&ctx), CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(1.0, 1.0)));
    objc2_core_graphics::CGBitmapContextCreateImage(Some(&ctx))
}

impl HasCoverage for Picture {
    fn coverage(&self) -> Coverage {
        self.coverage
    }
}

impl CarriesOver for Picture {
    /// A picture carrying its window's blur is kept over a capture of the window on its own at the
    /// same size, which would be flat grey exactly where the blur is. A different size means the
    /// window was resized and the old picture no longer fits it, so the new one is taken after all.
    fn keeps_over(&self, incoming: &Self) -> bool {
        keeps_blur(
            self.carries_blur,
            incoming.carries_blur,
            self.coverage,
            incoming.coverage,
        )
    }

    /// A stand-in is drawn and never kept: cached, the switcher would read it as a picture and the next
    /// flight would take it for one.
    fn cacheable(&self) -> bool {
        self.source != Route::StandIn
    }
}

/// A 1x1 bitmap for tests that need a real `CGImage` but never read its pixels.
#[cfg(test)]
pub(crate) fn test_bitmap() -> CFRetained<CGImage> {
    use objc2_core_graphics::{CGBitmapInfo, CGColorSpace, CGDataProvider, CGImageAlphaInfo};
    static PIXEL: [u8; 4] = [0, 0, 0, 255];
    // SAFETY: the data outlives the provider, being a static, so no release callback is needed.
    let provider = unsafe {
        CGDataProvider::with_data(
            std::ptr::null_mut(),
            PIXEL.as_ptr() as *const std::ffi::c_void,
            PIXEL.len(),
            None,
        )
    }
    .expect("data provider");
    let space = CGColorSpace::new_device_rgb().expect("colour space");
    // SAFETY: 1x1 BGRA, and the provider holds exactly those four bytes.
    unsafe {
        CGImage::new(
            1,
            1,
            8,
            32,
            4,
            Some(&space),
            CGBitmapInfo(CGImageAlphaInfo::PremultipliedLast.0),
            Some(&provider),
            std::ptr::null(),
            false,
            objc2_core_graphics::CGColorRenderingIntent::RenderingIntentDefault,
        )
    }
    .expect("image")
}

/// The right half of a 4x2 composite, as a region picture of a 2x2 window.
#[cfg(test)]
pub(crate) fn test_region() -> Picture {
    use rini_geometry::space::Point;

    let ctx = rgba_bitmap_context(4, 2).expect("a context");
    let composite = objc2_core_graphics::CGBitmapContextCreateImage(Some(&ctx)).expect("an image");
    Picture {
        image: Pixels::Region {
            composite,
            rect: PixelRect::new(Point::new(2.0, 0.0), Size::new(2.0, 2.0)),
        },
        ..test_picture(ScreenSize::new(2.0, 2.0))
    }
}

/// A usable picture that claims to cover a window of `size`, for tests about geometry.
#[cfg(test)]
pub(crate) fn test_picture<U: PointSpace>(size: Size<U>) -> Picture {
    use rini_geometry::space::Respace;
    Picture {
        image: Pixels::Bitmap(test_bitmap()),
        coverage: Coverage {
            covered: size.respace(),
            window: size.respace(),
        },
        source: Route::ScreenCaptureKit,
        taken: std::time::Instant::now(),
        carries_blur: false,
    }
}

#[cfg(test)]
mod tests {
    use rini_core::ids::WindowId;
    use rini_geometry::space::Point;

    use super::*;
    use crate::pictures::domain::cache::PictureCache;

    /// A stand-in never enters the cache, where the switcher would read it as a picture.
    #[test]
    fn a_stand_in_is_never_cached() {
        let mut cache: PictureCache<Picture> = PictureCache::new();
        let window = WindowId::new(1, 1);
        assert!(!cache.insert(
            window,
            stand_in_picture(ScreenSize::new(859.0, 1081.0), test_bitmap())
        ));
        assert!(cache.get(window).is_none());
    }

    /// The stand-in's pixels are one pixel of its colour, opaque, so its size never grows with the
    /// window's.
    #[test]
    fn the_stand_in_is_one_pixel_of_its_colour() {
        use objc2_core_graphics::CGContext;
        let image = stand_in_image().expect("drawn");
        assert_eq!(CGImage::width(Some(&image)), 1);
        assert_eq!(CGImage::height(Some(&image)), 1);
        let ctx = rgba_bitmap_context(1, 1).expect("a context");
        CGContext::draw_image(
            Some(&ctx),
            CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(1.0, 1.0)),
            Some(&image),
        );
        let data = objc2_core_graphics::CGBitmapContextGetData(Some(&ctx)) as *const u8;
        // SAFETY: the context's own buffer, one RGBA pixel.
        let pixel = unsafe { std::slice::from_raw_parts(data, 4) };
        let (r, g, b) = STAND_IN_RGB;
        for (got, want) in pixel.iter().zip([r, g, b, 1.0]) {
            assert!((*got as f64 - want * 255.0).abs() <= 1.0, "{pixel:?}");
        }
    }

    /// A region is kept as a copy of its own pixels, stamped as it was; any other kind is kept as it is.
    #[test]
    fn a_region_is_kept_as_a_copy_and_anything_else_as_it_is() {
        let region = test_region();
        let kept = region.kept().expect("a copy");
        assert!(matches!(kept.image, Pixels::Bitmap(_)));
        assert_eq!(kept.taken, region.taken);
        assert_eq!(kept.coverage, region.coverage);
        assert!(test_picture(ScreenSize::new(1.0, 1.0)).kept().is_none());
    }

    /// A region's own image is a crop of its rect, and its unit rect is that rect over the composite.
    #[test]
    fn a_region_is_its_rect_of_the_composite() {
        let region = test_region();
        let crop = region.image.bitmap().expect("a crop");
        assert_eq!((CGImage::width(Some(&crop)), CGImage::height(Some(&crop))), (2, 2));
        assert_eq!(
            region.image.unit_rect(),
            ContentsRect::new(Point::new(0.5, 0.0), Size::new(0.5, 1.0))
        );
        assert_eq!(test_picture(ScreenSize::new(1.0, 1.0)).image.unit_rect(), WHOLE);
    }

    /// The reported flicker: a blurred window's picture, taken on screen with its blur, must not be
    /// replaced by a later capture of the window on its own, which is flat grey where the blur is.
    #[test]
    fn a_picture_with_its_blur_is_kept_over_a_grey_one_of_the_same_size() {
        let mut cache: PictureCache<Picture> = PictureCache::new();
        let window = WindowId::new(1, 1);
        let mut blurred = test_picture(ScreenSize::new(859.0, 1081.0));
        blurred.carries_blur = true;
        assert!(cache.insert(window, blurred), "a first picture is held");

        assert!(
            !cache.insert(window, test_picture(ScreenSize::new(859.0, 1081.0))),
            "and the refusal is reported, so a flight in the air does not draw the grey one either"
        );
        assert!(cache.get(window).unwrap().carries_blur, "the blur stays");
    }

    #[test]
    fn forgetting_a_window_empties_the_cache_even_while_a_clone_is_held() {
        let window = WindowId::new(1, 1);
        let mut cache: PictureCache<Picture> = PictureCache::new();
        let size = ScreenSize::new(859.0, 1081.0);
        cache.insert(window, test_picture(size));
        let held = cache.usable(window).cloned().expect("inserted");
        cache.forget(window);
        assert!(cache.usable(window).is_none());
        assert!(held.is_usable());
        assert!(held.fits(ScreenSize::new(859.0, 1081.0)));
    }
}
