//! Every SkyLight capture: one composite of a list of windows, the batched capture
//! of the windows on screen as regions of it, the framed capture of one window with, wholly on a
//! display, its blur, and a handover check's two: a window alone and the whole screen.
//!
//! SkyLight captures what is on screen, fresh. Constraints and costs:
//! `src/pictures/docs/capture-research.md`.

use std::collections::HashMap;
use std::ffi::c_int;
use std::time::Instant;

use objc2_core_foundation::{CFArray, CFRetained, CGPoint, CGRect, CGSize};
use objc2_core_graphics::{CGError, CGImage};
use rini_core::ids::{WindowId, WindowServerId, pid_t};
use rini_geometry::space::{FromCg, PixelRect, ScreenPoint, ScreenRect, ScreenSize, ToCg};
use rini_skylight_sys::{SLSCaptureWindowsContentsToRectWithOptions, SLSHWCaptureWindowList};

use crate::pictures::domain::coverage::Coverage;
use crate::pictures::domain::knowledge::{Call, Learned};
use crate::pictures::domain::region::{composite_frame, composite_scale, contains, pixel_rect};
use crate::pictures::domain::route::Route;
use crate::pictures::platform::bitmap::rgba_bitmap_context;
use crate::pictures::platform::desktop::DesktopBackdrop;
use crate::pictures::platform::measure::{first_sight, shows_material};
use crate::pictures::platform::picture::{LazyGround, Picture, Pixels, image_size};
use crate::windows::platform::window_server::{READ_CONNECTION, WindowServer};

/// Undocumented `SLSHWCaptureWindowList` option bits, as yabai passes them. See "What yabai
/// actually does" in `src/animation/docs/capture-overlay-research.md`.
const CAPTURE_OPTIONS: u32 = (1 << 11) | (1 << 8);

/// One `SLSHWCaptureWindowList` call: one composite of `windows`, the first listed in front, covering
/// what each has on its display. The only SkyLight capture call; every picture of a list of windows is
/// cut from what it returns.
fn composite(windows: &[WindowServerId]) -> Option<CFRetained<CGImage>> {
    if windows.is_empty() {
        return None;
    }
    let ids: Vec<u32> = windows.iter().map(|w| w.as_u32()).collect();
    // SAFETY: `ids` holds the `ids.len()` window numbers the call is told of and outlives it, on the
    // read connection, which lives for the process.
    let array: *mut CFArray<CGImage> = unsafe {
        SLSHWCaptureWindowList(
            *READ_CONNECTION,
            ids.as_ptr(),
            ids.len() as c_int,
            CAPTURE_OPTIONS,
        )
    };
    // SAFETY: SLSHWCaptureWindowList returns a +1 CFArray of CGImage, so ownership transfers here.
    let array = unsafe { CFRetained::from_raw(std::ptr::NonNull::new(array)?) };
    array.iter().next()
}

/// One composite of several windows, as one picture: the desktop backdrop, and another app's bar.
pub fn capture_list_via_skylight(
    windows: &[WindowServerId],
    covers: ScreenSize,
    scale: f64,
) -> Option<Picture> {
    let image = composite(windows)?;
    let scale = if scale > 0.0 { scale } else { 1.0 };
    let pixels = image_size(&image);
    Some(Picture {
        image: Pixels::Bitmap(image),
        coverage: Coverage {
            covered: ScreenSize::new(pixels.width / scale, pixels.height / scale),
            window: covers,
        },
        source: Route::SkyLight,
        taken: std::time::Instant::now(),
        carries_blur: false,
        ground: LazyGround::default(),
    })
}

/// One window alone, as SkyLight has it now: what the overlay draws, for a handover check.
pub fn capture_alone(window: WindowServerId) -> Option<CFRetained<CGImage>> { composite(&[window]) }

/// What `display` shows, every window on it, the overlay included while it is up.
pub fn capture_screen(
    server: &dyn WindowServer,
    display: ScreenRect,
) -> Option<CFRetained<CGImage>> {
    let ids: Vec<u32> = server
        .on_screen_windows(false)
        .iter()
        .map(|window| window.id.as_u32())
        .collect();
    capture_rect(&ids, display.to_cg(), ON_SCREEN_OPTIONS).ok().flatten()
}

/// `SLSCaptureWindowsContentsToRectWithOptions` bits for windows wholly on a display: the picture as
/// the screen composites it. Off a display they capture only what is on it, so a window that is not
/// wholly on one is captured with none.
/// Measured in "The read connection answers as the main one does" in
/// `src/windows/docs/window-server-reads.md`.
const ON_SCREEN_OPTIONS: u32 = (1 << 11) | (1 << 8);

/// The options a framed capture of a window takes, by whether it is wholly on a display.
fn framed_options(wholly_on_a_display: bool) -> u32 {
    if wholly_on_a_display {
        ON_SCREEN_OPTIONS
    } else {
        0
    }
}

/// One picture of the windows in `ids`, the first listed in front, over `rect` in global points or,
/// with `CGRectNull`, the union of their bounds; `Ok(None)` when the call made none.
pub fn capture_rect(
    ids: &[u32],
    rect: CGRect,
    options: u32,
) -> Result<Option<CFRetained<CGImage>>, CGError> {
    let mut image: *mut CGImage = std::ptr::null_mut();
    // SAFETY: `ids` outlives the call and holds `ids.len()` window ids; the image comes back retained.
    let err = unsafe {
        SLSCaptureWindowsContentsToRectWithOptions(
            *READ_CONNECTION,
            ids.as_ptr(),
            ids.len() as c_int,
            rect,
            options,
            &mut image,
        )
    };
    // SAFETY: a non-null image is ours to release.
    let image = std::ptr::NonNull::new(image).map(|image| unsafe { CFRetained::from_raw(image) });
    if err != CGError::Success {
        return Err(err);
    }
    Ok(image)
}

/// `window` and every window listed behind it, front to back; `None` when it is not listed.
fn from_window_back(listed: &[u32], window: u32) -> Option<&[u32]> {
    let at = listed.iter().position(|&id| id == window)?;
    Some(&listed[at..])
}

/// A window to capture, at the frame the window server has it at.
#[derive(Clone, Copy, Debug)]
pub struct OnScreen {
    pub window: WindowId,
    pub server_id: WindowServerId,
    pub frame: CGRect,
}

/// One batched capture asked: the windows to capture, what lies behind them, the desktop listed after
/// them, the display they are on, and the process the window server has holding the keys.
#[derive(Clone, Copy)]
pub struct BatchCall<'a> {
    pub windows: &'a [OnScreen],
    pub behind: &'a [(WindowServerId, CGRect)],
    pub desktop: &'a DesktopBackdrop,
    pub display: CGRect,
    pub key: Option<pid_t>,
}

/// What one batched capture brought back.
pub struct Batch {
    pub pictures: Vec<(WindowId, Picture)>,
    /// The baked call came back another shape than its windows, the desktop listed after them
    /// included: a desktop window listed is no longer there.
    pub desktop_stale: bool,
    /// The windows whose call `halt` stopped before it was made.
    pub halted: Vec<WindowId>,
}

/// The windows in `asked.windows`, wholly on its display at their server frames and none overlapping
/// another, each as a region of the call it was captured in. What is known of each decides the call
/// (`crate::pictures::domain::knowledge`):
///
/// - the alpha call, the windows alone, keeps a translucent window translucent, so the overlay draws
///   what is really behind it; a window seen for the first time is measured from it;
/// - the baked call lists the opaque windows, then `behind`, then the desktop, so a material carries the
///   blur of what is below it. A window opaque and of unknown material is in both, and its material is
///   learned from the difference, as far as the window is active: an inactive window draws its
///   material flat, so finding none there is not an answer (`Learned::saw_material`), and `key`, the
///   process the window server has holding the keys, is what says it is active.
///
/// A composite of another shape than its windows yields no picture. Measured in "A batched call
/// composites in the order given" in `src/pictures/docs/capture-research.md`. `halt` is asked before
/// each call, and once it says stop no call is made.
pub fn capture_on_screen(
    asked: BatchCall<'_>,
    learned: &mut Learned,
    halt: &dyn Fn() -> bool,
) -> Batch {
    let BatchCall {
        windows,
        behind,
        desktop,
        display,
        key,
    } = asked;
    let taken = Instant::now();
    let mut pictures = Vec::new();
    let (alpha, mut baked): (Vec<OnScreen>, Vec<OnScreen>) = windows
        .iter()
        .partition(|window| learned.get(window.server_id).first_call() == Call::Alpha);
    let mut alone = HashMap::new();
    let mut halted = Vec::new();
    if !alpha.is_empty() && halt() {
        halted.extend(alpha.iter().chain(&baked).map(|window| window.window));
        return Batch {
            pictures,
            desktop_stale: false,
            halted,
        };
    }
    if let Some((composite, origin, scale)) = call(&alpha, &[], None, display) {
        for window in alpha {
            let Some((rect, crop)) = cut(&composite, origin, scale, window.frame) else {
                continue;
            };
            if learned.get(window.server_id).translucent.is_none() {
                learned.saw_alpha(window.server_id, first_sight(&crop));
            }
            match learned.get(window.server_id).after_alpha(learned.drawn(
                window.window,
                key,
                taken,
            )) {
                Call::Alpha => {
                    pictures.push(region(&window, &composite, rect, taken, Route::SkyLight, false))
                }
                Call::Baked => {
                    alone.insert(window.server_id, crop);
                    baked.push(window);
                }
            }
        }
    }
    let mut desktop_stale = false;
    if !baked.is_empty() && halt() {
        halted.extend(baked.iter().map(|window| window.window));
    } else if !baked.is_empty() {
        match call(&baked, behind, Some(desktop), display) {
            Some((composite, origin, scale)) => {
                for window in baked {
                    let Some((rect, crop)) = cut(&composite, origin, scale, window.frame) else {
                        continue;
                    };
                    if let Some(alone) = alone.get(&window.server_id) {
                        let drawn = learned.drawn(window.window, key, taken);
                        learned.saw_material(window.server_id, shows_material(alone, &crop), drawn);
                    }
                    let material = learned.get(window.server_id).material == Some(true);
                    pictures.push(region(&window, &composite, rect, taken, Route::Baked, material));
                }
            }
            None => desktop_stale = true,
        }
    }
    Batch {
        pictures,
        desktop_stale,
        halted,
    }
}

/// One call for `windows`, then `behind` not already listed, then the desktop when given, and where
/// the composite it returns lies: its top-left in global points and its scale. `None` when it is not
/// the shape of what was listed.
fn call(
    windows: &[OnScreen],
    behind: &[(WindowServerId, CGRect)],
    desktop: Option<&DesktopBackdrop>,
    display: CGRect,
) -> Option<(CFRetained<CGImage>, CGPoint, f64)> {
    if windows.is_empty() {
        return None;
    }
    let mut ids: Vec<WindowServerId> = windows.iter().map(|window| window.server_id).collect();
    let mut frames: Vec<ScreenRect> =
        windows.iter().map(|window| ScreenRect::from_cg(window.frame)).collect();
    for (id, frame) in behind {
        if !ids.contains(id) {
            ids.push(*id);
            frames.push(ScreenRect::from_cg(*frame));
        }
    }
    if let Some(desktop) = desktop {
        ids.extend(&desktop.windows);
        frames.extend(desktop.bounds);
    }
    let image = composite(&ids)?;
    let covers = composite_frame(&frames, ScreenRect::from_cg(display))?;
    let scale = composite_scale(covers, image_size(&image))?;
    Some((image, covers.origin.to_cg(), scale))
}

/// The pixel rect of a window at `frame` in a composite whose top-left is `origin`, and its crop, if the
/// composite holds it.
fn cut(
    composite: &CGImage,
    origin: CGPoint,
    scale: f64,
    frame: CGRect,
) -> Option<(PixelRect, CFRetained<CGImage>)> {
    let rect = pixel_rect(ScreenRect::from_cg(frame), ScreenPoint::from_cg(origin), scale);
    if !contains(image_size(composite), rect) {
        return None;
    }
    Some((rect, CGImage::with_image_in_rect(Some(composite), rect.to_cg())?))
}

fn region(
    window: &OnScreen,
    composite: &CFRetained<CGImage>,
    rect: PixelRect,
    taken: Instant,
    source: Route,
    carries_blur: bool,
) -> (WindowId, Picture) {
    let size = ScreenSize::from_cg(window.frame.size);
    (window.window, Picture {
        image: Pixels::Region {
            composite: composite.clone(),
            rect,
        },
        coverage: Coverage { covered: size, window: size },
        source,
        taken,
        carries_blur,
        ground: LazyGround::default(),
    })
}

/// One framed capture of a window: a new window's at its spawn frame. See "A window that opens" in
/// `src/transitions/docs/coordination.md`.
///
/// For a window wholly on a display, the picture also gets its blur back: a second capture of the
/// window with everything below it, merged in by `translucency::merge`. About 14ms, measured, which is
/// why it is never taken for every tile at the start of a flight.
pub fn capture_via_framed(
    server: &dyn WindowServer,
    window: WindowServerId,
    scale: f64,
) -> Option<Picture> {
    let frame = server.get_window(window)?.frame;
    if frame.size.width <= 0.0 || frame.size.height <= 0.0 || scale <= 0.0 {
        return None;
    }
    let displays = active_display_bounds();
    let on_a_display = fully_on_a_display(frame, &displays);
    let framed = capture_window_rect(window, frame, scale, on_a_display)?;
    let pixels = image_size(&framed);
    let composite = on_a_display
        .then(|| capture_below_and_including(server, window, frame))
        .flatten();
    let after = server.get_window(window).map(|w| w.frame);
    let whole = stayed_wholly_on_a_display(frame, after, &displays);
    let composite = composite.filter(|_| whole);
    let (image, carries_blur) = match composite.and_then(|composite| with_blur(&framed, &composite))
    {
        Some((merged, merge)) if merge.carries_blur => (merged, true),
        _ => (framed, false),
    };
    Some(Picture {
        image: Pixels::Bitmap(image),
        coverage: framed_coverage(
            ScreenSize::new(pixels.width / scale, pixels.height / scale),
            ScreenSize::from_cg(frame.size),
            whole,
        ),
        source: Route::SkyLight,
        taken: std::time::Instant::now(),
        carries_blur,
        ground: LazyGround::default(),
    })
}

/// The window and everything below it on screen, cropped to `rect`. Leaves out every window above it,
/// so neither an overlapping window nor rini's own overlay can end up in the picture.
fn capture_below_and_including(
    server: &dyn WindowServer,
    window: WindowServerId,
    rect: CGRect,
) -> Option<CFRetained<CGImage>> {
    let listed: Vec<u32> = server
        .on_screen_windows(false)
        .iter()
        .map(|window| window.id.as_u32())
        .collect();
    let ids = from_window_back(&listed, window.as_u32())?;
    capture_rect(ids, rect, ON_SCREEN_OPTIONS).ok().flatten()
}

/// A test takes no framed capture: its window ids can name the developer's own windows. See "A unit
/// test must not read the live window server" in `docs/testing.md`.
#[cfg(test)]
fn capture_window_rect(
    _: WindowServerId,
    _: CGRect,
    _: f64,
    _: bool,
) -> Option<CFRetained<CGImage>> {
    None
}

/// One framed capture of the window's own rect, or `None` when the window server returned an image
/// of another size.
#[cfg(not(test))]
fn capture_window_rect(
    server_id: WindowServerId,
    frame: CGRect,
    scale: f64,
    wholly_on_a_display: bool,
) -> Option<CFRetained<CGImage>> {
    let options = framed_options(wholly_on_a_display);
    let framed = capture_rect(&[server_id.as_u32()], frame, options).ok().flatten()?;
    let pixels = image_size(&framed);
    if (pixels.width - frame.size.width * scale).abs() > 2.0
        || (pixels.height - frame.size.height * scale).abs() > 2.0
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
    // SAFETY: `ids` has room for the `ids.len()` displays the call may write, and `count` is a live
    // out-parameter.
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
/// window that stayed wholly on a display is covered.
fn framed_coverage(inner: ScreenSize, window: ScreenSize, whole: bool) -> Coverage {
    Coverage {
        covered: if whole { inner } else { ScreenSize::zero() },
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

    #[test]
    fn a_window_off_a_display_is_captured_without_the_on_screen_bits() {
        assert_eq!(framed_options(true), (1 << 11) | (1 << 8));
        assert_eq!(framed_options(false), 0);
    }

    #[test]
    fn below_and_including_is_the_window_and_everything_listed_behind_it() {
        let listed = [9, 7, 5, 3];
        assert_eq!(from_window_back(&listed, 7), Some(&[7, 5, 3][..]));
        assert_eq!(from_window_back(&listed, 3), Some(&[3][..]));
        assert_eq!(
            from_window_back(&listed, 4),
            None,
            "not on screen, so nothing to composite"
        );
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
        let size = ScreenSize::new(859.0, 1081.0);

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
