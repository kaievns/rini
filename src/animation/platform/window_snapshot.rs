//! Bitmap snapshots of windows, for the capture-based animation overlay.
//!
//! SkyLight captures what is on screen, fresh; ScreenCaptureKit serves everything else from a
//! background cache. Constraints and costs of both: `src/animation/docs/capture-overlay-research.md`.

use std::ffi::c_int;

use objc2_core_foundation::{CFArray, CFRetained, CGPoint, CGRect, CGSize};
use objc2_core_graphics::CGImage;
use objc2_io_surface::IOSurfaceRef;

use crate::pictures::domain::cache::{CarriesOver, HasCoverage, dressing_after_insert, keeps_blur};
use crate::pictures::domain::coverage::{Coverage, fits_frame};
use rini_core::ids::WindowServerId;
use rini_skylight_sys::{SLSHWCaptureWindowList, SLSMainConnectionID};

/// Undocumented `SLSHWCaptureWindowList` option bits, as yabai passes them. See "What yabai
/// actually does" in `src/animation/docs/capture-overlay-research.md`.
const CAPTURE_OPTIONS: u32 = (1 << 11) | (1 << 8);

/// A window's pixels, whichever API produced them. Core Animation accepts either form.
#[derive(Clone, Debug)]
pub enum SnapshotImage {
    /// CPU-side bitmap, from `SLSHWCaptureWindowList`.
    Bitmap(CFRetained<CGImage>),
    /// GPU-side surface, from ScreenCaptureKit; off the process's heap.
    Surface(CFRetained<IOSurfaceRef>),
}

// SAFETY: CGImage is immutable and IOSurface is shareable across threads and processes.
unsafe impl Send for SnapshotImage {}

/// A window's pixels, plus how much of the window they cover.
#[derive(Clone, Debug)]
pub struct WindowSnapshot {
    pub image: SnapshotImage,
    pub coverage: Coverage,
    pub source: SnapshotSource,
    /// The hairline this window wore when last seen composited; carried across cache refreshes by
    /// [`crate::pictures::domain::cache::PictureCache::insert`]. See
    /// [`crate::animation::platform::edge_dressing`].
    pub dressing: Option<crate::animation::platform::edge_dressing::EdgeDressing>,
    /// When the pixels were captured; staleness is a reason to re-warm.
    pub taken: std::time::Instant,
    /// Whether this picture carries a blur its window's own surface lacks: taken as a composite with
    /// what was below the window, and different from the window's own capture where it matters. A
    /// picture like this is not given up for a same-sized capture of the window alone, which would
    /// be flat grey exactly where the blur is. See [`crate::pictures::domain::translucency`].
    pub carries_blur: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SnapshotSource {
    /// Captured fresh from the framebuffer. Only valid for a fully visible window.
    SkyLight,
    /// Captured from the window's own surface. Valid at any visibility, but expensive.
    ScreenCaptureKit,
    /// Not a capture: the dark stand-in a window with no picture flies with, so its slot is never a
    /// hole. Never cached.
    Placeholder,
}

impl WindowSnapshot {
    pub fn is_usable(&self) -> bool {
        self.coverage.is_usable()
    }

    /// Can this picture be drawn at `size` without visibly distorting it? See [`fits_frame`].
    pub fn fits(&self, size: CGSize) -> bool {
        fits_frame(self.coverage.covered, (size.width, size.height))
    }
}

/// One `SLSHWCaptureWindowList` call, for several windows composited together.
fn capture_list_via_skylight(
    windows: &[WindowServerId],
    covers: (f64, f64),
    scale: f64,
) -> Option<WindowSnapshot> {
    if windows.is_empty() {
        return None;
    }
    let cid = unsafe { SLSMainConnectionID() };
    let ids: Vec<u32> = windows.iter().map(|w| w.as_u32()).collect();
    let array: *mut CFArray<CGImage> =
        unsafe { SLSHWCaptureWindowList(cid, ids.as_ptr(), ids.len() as c_int, CAPTURE_OPTIONS) };
    if array.is_null() {
        return None;
    }
    // SAFETY: SLSHWCaptureWindowList returns a +1 CFArray of CGImage, so ownership transfers here.
    let array = unsafe { CFRetained::from_raw(std::ptr::NonNull::new(array)?) };
    let image = array.iter().next()?;
    let scale = if scale > 0.0 { scale } else { 1.0 };
    let px_w = CGImage::width(Some(&image)) as f64;
    let px_h = CGImage::height(Some(&image)) as f64;
    Some(WindowSnapshot {
        image: SnapshotImage::Bitmap(image),
        coverage: Coverage {
            covered: (px_w / scale, px_h / scale),
            window: covers,
        },
        source: SnapshotSource::SkyLight,
        dressing: None,
        taken: std::time::Instant::now(),
        carries_blur: false,
    })
}

/// One framed capture that yields the picture and its hairline: the reveal chase's capture, and the
/// harvest after a flight. See "A grow holds, then reveals" in `src/animation/docs/animation-smoothness.md`.
///
/// For a window wholly on a display, the picture also gets its blur back: a second capture of the
/// window with everything below it, merged in by `translucency::merge`. About 14ms, measured, which is
/// why it is never taken for every tile at the start of a flight.
pub fn capture_via_framed_with_dressing(
    window: WindowServerId,
    scale: f64,
) -> Option<WindowSnapshot> {
    use crate::animation::platform::edge_dressing::{
        capture_ring_expanded, harvest_from_capture, picture_within_ring, ring_expanded,
    };
    let frame = crate::windows::platform::window_server::get_window(window)?.frame;
    if frame.size.width <= 0.0 || frame.size.height <= 0.0 || scale <= 0.0 {
        return None;
    }
    let framed = capture_ring_expanded(window, frame, scale)?;
    let px_w = CGImage::width(Some(&framed)) as f64;
    let px_h = CGImage::height(Some(&framed)) as f64;
    let inner = picture_within_ring(px_w, px_h, scale);
    // The hairline is harvested from the window's own capture, before any merge.
    let dressing = harvest_from_capture(&framed, frame.size, scale);
    let displays = active_display_bounds();
    let composite = fully_on_a_display(frame, &displays)
        .then(|| capture_below_and_including(window, ring_expanded(frame)))
        .flatten();
    let after = crate::windows::platform::window_server::get_window(window).map(|w| w.frame);
    let whole = stayed_wholly_on_a_display(frame, after, &displays);
    let composite = composite.filter(|_| whole);
    let (picture, carries_blur) =
        match composite.and_then(|composite| with_blur(&framed, &composite)) {
            Some((merged, merge)) if merge.carries_blur => (merged, true),
            _ => (framed.clone(), false),
        };
    let image = CGImage::with_image_in_rect(Some(&picture), inner)?;
    Some(WindowSnapshot {
        image: SnapshotImage::Bitmap(image),
        coverage: framed_coverage(
            (inner.size.width / scale, inner.size.height / scale),
            (frame.size.width, frame.size.height),
            whole,
        ),
        source: SnapshotSource::SkyLight,
        dressing,
        taken: std::time::Instant::now(),
        carries_blur,
    })
}

/// Whether `window` lies wholly on one display now, which is when a framed capture of it is whole.
pub fn is_wholly_on_a_display(window: WindowServerId) -> bool {
    crate::windows::platform::window_server::get_window(window)
        .is_some_and(|info| fully_on_a_display(info.frame, &active_display_bounds()))
}

/// The window and everything below it on screen, cropped to `rect`. Leaves out every window above it,
/// so neither an overlapping window nor rini's own overlay can end up in the picture.
fn capture_below_and_including(
    window: WindowServerId,
    rect: CGRect,
) -> Option<CFRetained<CGImage>> {
    use objc2_core_graphics::{CGWindowImageOption, CGWindowListOption};
    #[allow(deprecated)]
    objc2_core_graphics::CGWindowListCreateImage(
        rect,
        CGWindowListOption::OptionOnScreenBelowWindow | CGWindowListOption::OptionIncludingWindow,
        window.as_u32(),
        CGWindowImageOption::empty(),
    )
}

/// `own` with its blur put back from `composite`, and what the merge found. `None` when the two are
/// not the same size, which a window moving between the two captures can cause.
fn with_blur(
    own: &CGImage,
    composite: &CGImage,
) -> Option<(
    CFRetained<CGImage>,
    crate::pictures::domain::translucency::Merged,
)> {
    use crate::pictures::domain::translucency::{Pictures, merge};
    let (width, height) = (CGImage::width(Some(own)), CGImage::height(Some(own)));
    if CGImage::width(Some(composite)) != width || CGImage::height(Some(composite)) != height {
        return None;
    }
    let own_ctx = rgba_context(own, width, height)?;
    let composite_ctx = rgba_context(composite, width, height)?;
    let own_stride = objc2_core_graphics::CGBitmapContextGetBytesPerRow(Some(&own_ctx));
    let composite_stride = objc2_core_graphics::CGBitmapContextGetBytesPerRow(Some(&composite_ctx));
    let own_data = objc2_core_graphics::CGBitmapContextGetData(Some(&own_ctx)) as *mut u8;
    let composite_data =
        objc2_core_graphics::CGBitmapContextGetData(Some(&composite_ctx)) as *const u8;
    if own_data.is_null() || composite_data.is_null() {
        return None;
    }
    // SAFETY: both contexts were created at `width` x `height` with the strides read back above, own
    // their buffers, and outlive these slices.
    let (own_bytes, composite_bytes) = unsafe {
        (
            std::slice::from_raw_parts_mut(own_data, own_stride * height),
            std::slice::from_raw_parts(composite_data, composite_stride * height),
        )
    };
    let merged = merge(Pictures {
        own: own_bytes,
        own_stride,
        composite: composite_bytes,
        composite_stride,
        width,
        height,
    })?;
    let image = objc2_core_graphics::CGBitmapContextCreateImage(Some(&own_ctx))?;
    Some((image, merged))
}

/// `image` drawn into a fresh RGBA context of its own size, alpha last and premultiplied. Premultiplied
/// is harmless to the merge, which only rewrites fully opaque pixels.
fn rgba_context(
    image: &CGImage,
    width: usize,
    height: usize,
) -> Option<CFRetained<objc2_core_graphics::CGContext>> {
    use objc2_core_graphics::CGContext;
    let ctx = crate::animation::platform::edge_dressing::rgba_bitmap_context(width, height)?;
    CGContext::draw_image(
        Some(&ctx),
        CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(width as f64, height as f64)),
        Some(image),
    );
    Some(ctx)
}

/// Every active display's bounds, in the window server's coordinates.
fn active_display_bounds() -> Vec<CGRect> {
    let mut ids = [0u32; 16];
    let mut count = 0u32;
    let err = unsafe {
        objc2_core_graphics::CGGetActiveDisplayList(ids.len() as u32, ids.as_mut_ptr(), &mut count)
    };
    if err != objc2_core_graphics::CGError::Success {
        return Vec::new();
    }
    ids[..count as usize]
        .iter()
        .map(|id| objc2_core_graphics::CGDisplayBounds(*id))
        .collect()
}

/// Whether the window was where it was read, wholly on one display, when its captures were taken. A
/// composite of a window that moved shows what it was covering; a framed capture of a window partly
/// off every display is transparent there.
fn stayed_wholly_on_a_display(before: CGRect, after: Option<CGRect>, displays: &[CGRect]) -> bool {
    after.is_some_and(|after| after == before && fully_on_a_display(after, displays))
}

/// What a framed capture covers. The window server returns the full requested rect whatever is on
/// screen, with the part off every display transparent, so the image's size says nothing: only a
/// window that stayed wholly on a display is covered. The rest is a hairline to harvest, not a
/// picture to draw.
fn framed_coverage(inner: (f64, f64), window: (f64, f64), whole: bool) -> Coverage {
    Coverage {
        covered: if whole { inner } else { (0.0, 0.0) },
        window,
    }
}

/// Whether `frame` lies wholly inside one display. Only then is there a screen below all of it for the
/// composite to have blurred; a window parked off the edge would come back part-empty.
fn fully_on_a_display(frame: CGRect, displays: &[CGRect]) -> bool {
    displays.iter().any(|display| {
        frame.origin.x >= display.origin.x
            && frame.origin.y >= display.origin.y
            && frame.origin.x + frame.size.width <= display.origin.x + display.size.width
            && frame.origin.y + frame.size.height <= display.origin.y + display.size.height
    })
}

/// `--n3`, the raised plane of the Okibi dark theme: the same stand-in the switcher draws for a row
/// with no picture.
const PLACEHOLDER_RGB: (f64, f64, f64) = (0.133, 0.145, 0.153);

/// The stand-in for a window of `window` size with no picture.
///
/// Claims to cover NOTHING of the window, which is honest and is also what makes a flight treat it as
/// a picture waiting for its reveal: the reveal chase follows the window, and the real picture replaces
/// this one before the flight moves or, mid-flight, by the reveal swap.
pub fn placeholder(window: CGSize, image: CFRetained<CGImage>) -> WindowSnapshot {
    WindowSnapshot {
        image: SnapshotImage::Bitmap(image),
        coverage: Coverage {
            covered: (0.0, 0.0),
            window: (window.width, window.height),
        },
        source: SnapshotSource::Placeholder,
        dressing: None,
        taken: std::time::Instant::now(),
        carries_blur: false,
    }
}

/// The placeholder's pixels: a small rounded square in `PLACEHOLDER_RGB`, drawn nine-slice by the
/// overlay so its corners keep the window's radius at any size. `(image, corner)`, the corner in pixels.
///
/// A small image rather than one at the window's size: a full-size bitmap of one flat colour would be
/// 30MB for a maximised window at 2x.
pub fn placeholder_image(scale: f64) -> Option<(CFRetained<CGImage>, f64)> {
    use objc2_core_graphics::CGContext;
    let corner = (crate::animation::platform::edge_dressing::CORNER_RADIUS * scale).ceil();
    let side = (corner * 2.0 + 2.0) as usize;
    let ctx = crate::animation::platform::edge_dressing::rgba_bitmap_context(side, side)?;
    let rect = CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(side as f64, side as f64));
    let path = unsafe {
        objc2_core_graphics::CGPath::with_rounded_rect(rect, corner, corner, std::ptr::null())
    };
    CGContext::add_path(Some(&ctx), Some(&path));
    let (r, g, b) = PLACEHOLDER_RGB;
    CGContext::set_rgb_fill_color(Some(&ctx), r, g, b, 1.0);
    CGContext::fill_path(Some(&ctx));
    Some((
        objc2_core_graphics::CGBitmapContextCreateImage(Some(&ctx))?,
        corner,
    ))
}

impl HasCoverage for WindowSnapshot {
    fn coverage(&self) -> Coverage {
        self.coverage
    }
}

impl CarriesOver for WindowSnapshot {
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
        self.source != SnapshotSource::Placeholder
    }
}

/// Captures several windows as one composited image, for the backdrop.
pub fn capture_composite_via_skylight(
    windows: &[WindowServerId],
    covers: (f64, f64),
    scale: f64,
) -> Option<WindowSnapshot> {
    capture_list_via_skylight(windows, covers, scale)
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

/// A usable snapshot that claims to cover a window of `size`, for tests about geometry.
#[cfg(test)]
pub(crate) fn test_snapshot(size: CGSize) -> WindowSnapshot {
    WindowSnapshot {
        image: SnapshotImage::Bitmap(test_bitmap()),
        coverage: Coverage {
            covered: (size.width, size.height),
            window: (size.width, size.height),
        },
        source: SnapshotSource::ScreenCaptureKit,
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
        let mut cache: PictureCache<WindowSnapshot> = PictureCache::new();
        let window = WindowId::new(1, 1);
        assert!(!cache.insert(window, placeholder(CGSize::new(859.0, 1081.0), test_bitmap())));
        assert!(cache.get(window).is_none());
    }

    /// The stand-in's pixels are a small square: two corners at the window's radius and a middle to
    /// stretch, so its size does not grow with the window's.
    #[test]
    fn the_stand_in_image_is_two_corners_and_a_middle() {
        let (image, corner) = placeholder_image(2.0).expect("drawn");
        assert_eq!(corner, 20.0, "10pt at 2x");
        assert_eq!(CGImage::width(Some(&image)), 42);
        assert_eq!(CGImage::height(Some(&image)), 42);
    }

    /// The reported flicker: a blurred window's picture, taken on screen with its blur, must not be
    /// replaced by a later capture of the window on its own, which is flat grey where the blur is.
    #[test]
    fn a_picture_with_its_blur_is_kept_over_a_grey_one_of_the_same_size() {
        let mut cache: PictureCache<WindowSnapshot> = PictureCache::new();
        let window = WindowId::new(1, 1);
        let mut blurred = test_snapshot(CGSize::new(859.0, 1081.0));
        blurred.carries_blur = true;
        assert!(cache.insert(window, blurred), "a first picture is held");

        assert!(
            !cache.insert(window, test_snapshot(CGSize::new(859.0, 1081.0))),
            "and the refusal is reported, so a flight in the air does not draw the grey one either"
        );
        assert!(cache.get(window).unwrap().carries_blur, "the blur stays");
    }

    /// The reported failure: the window was parked between reading its frame and taking the composite,
    /// so the rect showed another window. A composite is only kept if the window is where it was.
    #[test]
    fn a_capture_of_a_window_that_moved_is_thrown_away() {
        let laptop = CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(1728.0, 1117.0));
        let at = |x, y| CGRect::new(CGPoint::new(x, y), CGSize::new(859.0, 1081.0));

        assert!(stayed_wholly_on_a_display(
            at(4.0, 32.0),
            Some(at(4.0, 32.0)),
            &[laptop]
        ));
        assert!(
            !stayed_wholly_on_a_display(at(4.0, 32.0), Some(at(1727.0, 1065.0)), &[laptop]),
            "parked"
        );
        assert!(
            !stayed_wholly_on_a_display(at(4.0, 32.0), Some(at(10.0, 32.0)), &[laptop]),
            "moved"
        );
        assert!(
            !stayed_wholly_on_a_display(at(4.0, 32.0), None, &[laptop]),
            "gone"
        );
    }

    /// The reported regression: during a strip pan the real windows move while the flight flies, and
    /// a framed capture of one half past the display edge came back full-size, half transparent, and
    /// was cut onto its tile as a half-missing window. It must not count as covering the window.
    #[test]
    fn a_framed_capture_of_a_window_not_wholly_on_a_display_covers_nothing() {
        let laptop = CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(1728.0, 1117.0));
        let half_off = CGRect::new(CGPoint::new(1300.0, 32.0), CGSize::new(859.0, 1081.0));
        let slot = CGRect::new(CGPoint::new(4.0, 32.0), CGSize::new(859.0, 1081.0));
        let size = (859.0, 1081.0);

        let whole = stayed_wholly_on_a_display(slot, Some(slot), &[laptop]);
        assert!(framed_coverage(size, size, whole).is_usable());

        for (before, after, why) in [
            (half_off, Some(half_off), "half past the edge"),
            (slot, Some(half_off), "moved during the capture"),
        ] {
            let whole = stayed_wholly_on_a_display(before, after, &[laptop]);
            let coverage = framed_coverage(size, size, whole);
            assert!(!coverage.is_usable(), "{why}");
            assert!(!fits_frame(coverage.covered, size), "{why}");
        }
    }

    /// Only a window wholly on one display has a screen below all of it to have blurred; a parked
    /// window's composite would come back part-empty.
    #[test]
    fn only_a_window_wholly_on_one_display_is_composited() {
        let laptop = CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(1728.0, 1117.0));
        let external = CGRect::new(CGPoint::new(-670.0, -1692.0), CGSize::new(3008.0, 1692.0));
        let displays = [laptop, external];
        let at = |x, y, w, h| CGRect::new(CGPoint::new(x, y), CGSize::new(w, h));

        assert!(fully_on_a_display(at(4.0, 32.0, 1720.0, 1081.0), &displays));
        assert!(fully_on_a_display(at(0.0, -1600.0, 1200.0, 900.0), &displays));
        assert!(
            !fully_on_a_display(at(1727.0, 1085.0, 859.0, 1081.0), &displays),
            "parked"
        );
        assert!(
            !fully_on_a_display(at(100.0, -400.0, 800.0, 800.0), &displays),
            "straddles two"
        );
        assert!(
            !fully_on_a_display(at(4.0, 32.0, 100.0, 100.0), &[]),
            "no displays known"
        );
    }
}
