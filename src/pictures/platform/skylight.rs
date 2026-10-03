//! Every SkyLight and `CGWindowList` capture: one composite of a list of windows, and the framed
//! capture of one window with its hairline and, wholly on a display, its blur.
//!
//! SkyLight captures what is on screen, fresh. Constraints and costs:
//! `src/animation/docs/capture-overlay-research.md`.

use std::ffi::c_int;

use objc2_core_foundation::{CFArray, CFRetained, CGPoint, CGRect, CGSize};
use objc2_core_graphics::{CGImage, CGWindowListOption};
use rini_core::ids::WindowServerId;
use rini_skylight_sys::{SLSHWCaptureWindowList, SLSMainConnectionID};

use crate::pictures::domain::coverage::Coverage;
use crate::pictures::platform::bitmap::rgba_bitmap_context;
use crate::pictures::platform::edge_dressing::{
    harvest_from_capture, picture_within_ring, ring_expanded,
};
use crate::pictures::platform::picture::{Picture, Pixels, Route};

/// Undocumented `SLSHWCaptureWindowList` option bits, as yabai passes them. See "What yabai
/// actually does" in `src/animation/docs/capture-overlay-research.md`.
const CAPTURE_OPTIONS: u32 = (1 << 11) | (1 << 8);

/// One `SLSHWCaptureWindowList` call, for several windows composited together: the desktop backdrop,
/// and another app's bar.
pub fn capture_list_via_skylight(
    windows: &[WindowServerId],
    covers: (f64, f64),
    scale: f64,
) -> Option<Picture> {
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
    Some(Picture {
        image: Pixels::Bitmap(image),
        coverage: Coverage {
            covered: (px_w / scale, px_h / scale),
            window: covers,
        },
        source: Route::SkyLight,
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
) -> Option<Picture> {
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
    Some(Picture {
        image: Pixels::Bitmap(image),
        coverage: framed_coverage(
            (inner.size.width / scale, inner.size.height / scale),
            (frame.size.width, frame.size.height),
            whole,
        ),
        source: Route::SkyLight,
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
    use objc2_core_graphics::CGWindowImageOption;
    #[allow(deprecated)]
    objc2_core_graphics::CGWindowListCreateImage(
        rect,
        CGWindowListOption::OptionOnScreenBelowWindow | CGWindowListOption::OptionIncludingWindow,
        window.as_u32(),
        CGWindowImageOption::empty(),
    )
}

/// One framed capture of the window plus its ring, or `None` when the window server returned an
/// image of another size.
fn capture_ring_expanded(
    server_id: WindowServerId,
    frame: CGRect,
    scale: f64,
) -> Option<CFRetained<CGImage>> {
    let expanded = ring_expanded(frame);
    #[allow(deprecated)]
    let framed = objc2_core_graphics::CGWindowListCreateImage(
        expanded,
        CGWindowListOption::OptionIncludingWindow,
        server_id.as_u32(),
        objc2_core_graphics::CGWindowImageOption::empty(),
    )?;
    let px_w = CGImage::width(Some(&framed)) as f64;
    let px_h = CGImage::height(Some(&framed)) as f64;
    if (px_w - expanded.size.width * scale).abs() > 2.0
        || (px_h - expanded.size.height * scale).abs() > 2.0
    {
        return None;
    }
    Some(framed)
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
    let ctx = rgba_bitmap_context(width, height)?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pictures::domain::coverage::fits_frame;

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
