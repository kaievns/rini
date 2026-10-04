//! A picture of a window, the desktop or the bar: its pixels, how much of its window they cover, and
//! the route that took them. The stand-in a window with no picture flies with, and what the cache
//! keeps of a picture across captures.
//!
//! SkyLight captures what is on screen, fresh (`skylight`); ScreenCaptureKit serves everything else in
//! the background (`screencapturekit`). Constraints and costs of both:
//! `src/animation/docs/capture-overlay-research.md`.

use objc2_core_foundation::{CFRetained, CGPoint, CGRect, CGSize};
use objc2_core_graphics::CGImage;
use objc2_io_surface::IOSurfaceRef;

use crate::pictures::domain::cache::{CarriesOver, HasCoverage, dressing_after_insert, keeps_blur};
use crate::pictures::domain::coverage::{Coverage, fits_frame};
use crate::pictures::platform::bitmap::rgba_bitmap_context;
use crate::pictures::platform::edge_dressing::{CORNER_RADIUS, EdgeDressing};

/// A window's pixels, whichever API produced them. Core Animation accepts either form.
#[derive(Clone, Debug)]
pub enum Pixels {
    /// CPU-side bitmap, from `SLSHWCaptureWindowList`.
    Bitmap(CFRetained<CGImage>),
    /// GPU-side surface, from ScreenCaptureKit; off the process's heap.
    Surface(CFRetained<IOSurfaceRef>),
}

// SAFETY: CGImage is immutable and IOSurface is shareable across threads and processes.
unsafe impl Send for Pixels {}

/// A window's pixels, plus how much of the window they cover and how they were taken.
#[derive(Clone, Debug)]
pub struct Picture {
    pub image: Pixels,
    pub coverage: Coverage,
    pub source: Route,
    /// The hairline this window wore when last seen composited; carried across cache refreshes by
    /// [`crate::pictures::domain::cache::PictureCache::insert`]. See
    /// [`crate::pictures::platform::edge_dressing`].
    pub dressing: Option<EdgeDressing>,
    /// When the pixels were captured; staleness is a reason to re-warm.
    pub taken: std::time::Instant,
    /// Whether this picture carries a blur its window's own surface lacks: taken as a composite with
    /// what was below the window, and different from the window's own capture where it matters. A
    /// picture like this is not given up for a same-sized capture of the window alone, which would
    /// be flat grey exactly where the blur is. See [`crate::pictures::domain::translucency`].
    pub carries_blur: bool,
}

/// Which way a picture was taken.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Route {
    /// Captured fresh from the framebuffer. Only valid for a fully visible window.
    SkyLight,
    /// Captured from the window's own surface. Valid at any visibility, but expensive.
    ScreenCaptureKit,
    /// Not a capture: the dark stand-in a window with no picture flies with, so its slot is never a
    /// hole. Never cached.
    StandIn,
}

impl Picture {
    pub fn is_usable(&self) -> bool {
        self.coverage.is_usable()
    }

    /// Can this picture be drawn at `size` without visibly distorting it? See [`fits_frame`].
    pub fn fits(&self, size: CGSize) -> bool {
        fits_frame(self.coverage.covered, (size.width, size.height))
    }
}

/// `--n3`, the raised plane of the Okibi dark theme: the same stand-in the switcher draws for a row
/// with no picture.
const STAND_IN_RGB: (f64, f64, f64) = (0.133, 0.145, 0.153);

/// The stand-in for a window of `window` size with no picture.
///
/// Claims to cover NOTHING of the window, which is honest and is also what makes a flight treat it as
/// a picture waiting for its reveal: the reveal chase follows the window, and the real picture replaces
/// this one before the flight moves or, mid-flight, by the reveal swap.
pub fn stand_in_picture(window: CGSize, image: CFRetained<CGImage>) -> Picture {
    Picture {
        image: Pixels::Bitmap(image),
        coverage: Coverage {
            covered: (0.0, 0.0),
            window: (window.width, window.height),
        },
        source: Route::StandIn,
        dressing: None,
        taken: std::time::Instant::now(),
        carries_blur: false,
    }
}

/// The stand-in's pixels: a small rounded square in `STAND_IN_RGB`, drawn nine-slice by the
/// overlay so its corners keep the window's radius at any size. `(image, corner)`, the corner in pixels.
///
/// A small image rather than one at the window's size: a full-size bitmap of one flat colour would be
/// 30MB for a maximised window at 2x.
pub fn stand_in_image(scale: f64) -> Option<(CFRetained<CGImage>, f64)> {
    use objc2_core_graphics::CGContext;
    let corner = (CORNER_RADIUS * scale).ceil();
    let side = (corner * 2.0 + 2.0) as usize;
    let ctx = rgba_bitmap_context(side, side)?;
    let rect = CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(side as f64, side as f64));
    let path = unsafe {
        objc2_core_graphics::CGPath::with_rounded_rect(rect, corner, corner, std::ptr::null())
    };
    CGContext::add_path(Some(&ctx), Some(&path));
    let (r, g, b) = STAND_IN_RGB;
    CGContext::set_rgb_fill_color(Some(&ctx), r, g, b, 1.0);
    CGContext::fill_path(Some(&ctx));
    Some((
        objc2_core_graphics::CGBitmapContextCreateImage(Some(&ctx))?,
        corner,
    ))
}

impl HasCoverage for Picture {
    fn coverage(&self) -> Coverage {
        self.coverage
    }
}

impl CarriesOver for Picture {
    fn inherit(&mut self, previous: &Self) {
        self.dressing = dressing_after_insert(previous.dressing.clone(), self.dressing.take());
    }

    fn absorb(&mut self, refused: Self) {
        self.dressing = dressing_after_insert(self.dressing.take(), refused.dressing);
    }

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

    /// A stand-in is drawn and never kept: cached, it would be lent to the switcher as a picture and
    /// taken for one by the next flight.
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

/// A usable picture that claims to cover a window of `size`, for tests about geometry.
#[cfg(test)]
pub(crate) fn test_picture(size: CGSize) -> Picture {
    Picture {
        image: Pixels::Bitmap(test_bitmap()),
        coverage: Coverage {
            covered: (size.width, size.height),
            window: (size.width, size.height),
        },
        source: Route::ScreenCaptureKit,
        dressing: None,
        taken: std::time::Instant::now(),
        carries_blur: false,
    }
}

#[cfg(test)]
mod tests {
    use rini_core::ids::WindowId;

    use super::*;
    use crate::pictures::domain::cache::PictureCache;

    /// A stand-in never enters the cache, where it would be lent to the switcher as a picture.
    #[test]
    fn a_stand_in_is_never_cached() {
        let mut cache: PictureCache<Picture> = PictureCache::new();
        let window = WindowId::new(1, 1);
        assert!(!cache.insert(window, stand_in_picture(CGSize::new(859.0, 1081.0), test_bitmap())));
        assert!(cache.get(window).is_none());
    }

    /// The stand-in's pixels are a small square: two corners at the window's radius and a middle to
    /// stretch, so its size does not grow with the window's.
    #[test]
    fn the_stand_in_image_is_two_corners_and_a_middle() {
        let (image, corner) = stand_in_image(2.0).expect("drawn");
        assert_eq!(corner, 20.0, "10pt at 2x");
        assert_eq!(CGImage::width(Some(&image)), 42);
        assert_eq!(CGImage::height(Some(&image)), 42);
    }

    /// The reported flicker: a blurred window's picture, taken on screen with its blur, must not be
    /// replaced by a later capture of the window on its own, which is flat grey where the blur is.
    #[test]
    fn a_picture_with_its_blur_is_kept_over_a_grey_one_of_the_same_size() {
        let mut cache: PictureCache<Picture> = PictureCache::new();
        let window = WindowId::new(1, 1);
        let mut blurred = test_picture(CGSize::new(859.0, 1081.0));
        blurred.carries_blur = true;
        assert!(cache.insert(window, blurred), "a first picture is held");

        assert!(
            !cache.insert(window, test_picture(CGSize::new(859.0, 1081.0))),
            "and the refusal is reported, so a flight in the air does not draw the grey one either"
        );
        assert!(cache.get(window).unwrap().carries_blur, "the blur stays");
    }

    #[test]
    fn forgetting_a_window_empties_the_cache_even_while_a_clone_is_held() {
        let window = WindowId::new(1, 1);
        let mut cache: PictureCache<Picture> = PictureCache::new();
        let size = CGSize::new(859.0, 1081.0);
        cache.insert(window, test_picture(size));
        let held = cache.usable(window).cloned().expect("inserted");
        cache.forget(window);
        assert!(cache.usable(window).is_none());
        assert!(held.is_usable());
        assert!(held.fits(size));
    }
}
