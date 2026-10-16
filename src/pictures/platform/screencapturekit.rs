//! Window captures and the desktop render through ScreenCaptureKit, for what SkyLight cannot serve.
//!
//! Fills a cache rather than capturing on demand: a capture is too slow to run at switch time. Results
//! are `IOSurface` to keep a warm cache off the heap. When each call is made is the schedule's
//! (`crate::pictures::domain::schedule`); this only makes it. See
//! `src/pictures/docs/capture-research.md`.

use std::sync::Arc;

use block2::RcBlock;
use objc2::AnyThread;
use objc2::rc::Retained;
use objc2_core_foundation::{CFRetained, CGSize};
use objc2_core_media::CMSampleBuffer;
use objc2_core_video::{CVPixelBuffer, CVPixelBufferGetIOSurface};
use objc2_foundation::{NSArray, NSError};
use objc2_io_surface::IOSurfaceRef;
use objc2_screen_capture_kit::{
    SCCaptureResolutionType, SCContentFilter, SCScreenshotManager, SCShareableContent,
    SCStreamConfiguration, SCWindow,
};
use rini_geometry::space::{FromCg, ScreenSize, ToCg};
use tracing::{debug, warn};

use crate::pictures::domain::coverage::Coverage;
use crate::pictures::domain::route::Route;
use crate::pictures::domain::schedule::Display;
use crate::pictures::domain::target::PictureTarget;
use crate::pictures::platform::picture::{LazyGround, Picture, Pixels};
use crate::pictures::platform::surface::{Layout, new_surface, read_locked};

/// Alpha above which a pixel counts as painted; shadows are excluded, so a painted edge is opaque.
const PAINTED_ALPHA: u8 = 8;

/// Do both far edges of a capture have painted pixels? Several samples per edge, since a rounded
/// corner can leave any single point clear.
fn edges_are_painted(width: usize, height: usize, alpha_at: impl Fn(usize, usize) -> u8) -> bool {
    if width == 0 || height == 0 {
        return false;
    }
    /// How far in from an edge to sample, to clear a rounded corner.
    const INSET: usize = 4;
    let right = width.saturating_sub(1 + INSET);
    let bottom = height.saturating_sub(1 + INSET);
    let mut reaches_right = false;
    let mut reaches_bottom = false;
    for i in 1..8 {
        let y = height * i / 8;
        if y < height && alpha_at(right, y) > PAINTED_ALPHA {
            reaches_right = true;
        }
        let x = width * i / 8;
        if x < width && alpha_at(x, bottom) > PAINTED_ALPHA {
            reaches_bottom = true;
        }
    }
    reaches_right && reaches_bottom
}

/// Does a capture's content reach the far edge of its buffer? `None` means it could not be inspected.
/// See "Nominal capture resolution paints a quarter of the buffer" in `src/pictures/docs/capture-research.md`.
fn content_reaches_edges(buffer: &CVPixelBuffer) -> Option<bool> {
    use objc2_core_video::{
        CVPixelBufferGetBaseAddress, CVPixelBufferGetBytesPerRow, CVPixelBufferGetDataSize,
        CVPixelBufferGetHeight, CVPixelBufferGetWidth, CVPixelBufferLockBaseAddress,
        CVPixelBufferLockFlags, CVPixelBufferUnlockBaseAddress,
    };

    let width = CVPixelBufferGetWidth(buffer);
    let height = CVPixelBufferGetHeight(buffer);
    if width == 0 || height == 0 {
        return None;
    }
    // The pixel buffer's lock, not the IOSurface's: an IOSurface lock off the main thread races
    // SkyLight's lazy initialisation.
    // SAFETY: `buffer` is a live pixel buffer, unlocked below with the same flags.
    if unsafe { CVPixelBufferLockBaseAddress(buffer, CVPixelBufferLockFlags::ReadOnly) } != 0 {
        return None;
    }
    let stride = CVPixelBufferGetBytesPerRow(buffer);
    let base = CVPixelBufferGetBaseAddress(buffer) as *const u8;
    let length = stride.checked_mul(height);
    let painted = match length {
        Some(length)
            if !base.is_null()
                && stride >= width * 4
                && length <= CVPixelBufferGetDataSize(buffer) =>
        {
            // SAFETY: the buffer holds `stride * height` bytes at `base`, locked for reading, and
            // ScreenCaptureKit writes it no more once it has delivered it.
            let pixels = unsafe { std::slice::from_raw_parts(base, length) };
            Some(edges_are_painted(width, height, |x, y| {
                pixels[y * stride + x * 4 + 3]
            }))
        }
        _ => None,
    };
    // SAFETY: the lock taken above, with the same flags.
    unsafe { CVPixelBufferUnlockBaseAddress(buffer, CVPixelBufferLockFlags::ReadOnly) };
    painted
}

/// Copies a capture's pixels into an IOSurface this process owns: ScreenCaptureKit recycles its pool
/// surfaces once the completion returns. See "Cached surfaces have to be marked in use" in the research doc.
fn own_copy(source: &IOSurfaceRef) -> Option<CFRetained<IOSurfaceRef>> {
    let (width, height) = (source.width(), source.height());
    let stride = source.bytes_per_row();
    if width == 0 || height == 0 || stride < width * 4 {
        return None;
    }
    let layout = Layout {
        bytes_per_row: Some(stride),
        pixel_format: Some(source.pixel_format()),
    };
    new_surface(width, height, layout, |copy, copy_stride| {
        let row_bytes = stride.min(copy_stride);
        read_locked(source, |pixels| {
            for row in 0..height {
                copy[row * copy_stride..][..row_bytes]
                    .copy_from_slice(&pixels[row * stride..][..row_bytes]);
            }
        })
    })
}

/// What a listing prepared for capturing one window it found.
pub struct Prepared {
    /// The window's size at listing: what the capture is of. Mid-resize it differs from the target's.
    size: CGSize,
    filter: Retained<SCContentFilter>,
    config: Retained<SCStreamConfiguration>,
}

// SAFETY: the filter and configuration are immutable once built, and consumed only by a thread-safe
// class method.
unsafe impl Send for Prepared {}

/// The scale every window is captured at, whatever display it is on: ScreenCaptureKit charges per
/// call, not per pixel ("Capture cost" in `src/pictures/docs/capture-research.md`), and a
/// window captured at 1x would fly soft on a Retina display.
const WINDOW_SCALE: f64 = 2.0;

/// Whether a ScreenCaptureKit listing leaves out the desktop's own windows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DesktopWindows {
    Excluded,
    Included,
}

/// Asks ScreenCaptureKit for what it can capture, on screen or off, handing `listed` the listing, or
/// `None` when it gave none. `onScreenWindowsOnly` is false, or hidden-workspace windows are never
/// listed.
fn shareable_content(
    desktop: DesktopWindows,
    listed: impl Fn(Option<&SCShareableContent>) + Send + Sync + 'static,
) {
    let block = RcBlock::new(move |content: *mut SCShareableContent, _error: *mut NSError| {
        // SAFETY: ScreenCaptureKit hands its completion a listing that lives until it returns, or null.
        listed(unsafe { content.as_ref() });
    });
    // SAFETY: the block takes the listing and error the selector passes it, and what it holds is
    // `Send + Sync`, since ScreenCaptureKit calls it on a queue of its own.
    unsafe {
        SCShareableContent::getShareableContentExcludingDesktopWindows_onScreenWindowsOnly_completionHandler(
            desktop == DesktopWindows::Excluded,
            false,
            &block,
        );
    }
}

/// Takes one picture through `filter` as `config` sets it up, handing `taken` the pixel buffer
/// delivered, or `None` when none was.
fn screenshot(
    filter: &SCContentFilter,
    config: &SCStreamConfiguration,
    taken: impl Fn(Option<CFRetained<CVPixelBuffer>>) + Send + Sync + 'static,
) {
    let completion = RcBlock::new(move |sample: *mut CMSampleBuffer, _error: *mut NSError| {
        // SAFETY: ScreenCaptureKit hands its completion a sample buffer that lives until it returns,
        // or null, and `image_buffer` retains the buffer it returns, under the Get rule.
        taken(unsafe { sample.as_ref().and_then(|sample| sample.image_buffer()) });
    });
    // SAFETY: `filter` and `config` are live, and the block takes the sample and error the selector
    // passes it, holding only what is `Send + Sync`: it runs on ScreenCaptureKit's delivery queue.
    unsafe {
        SCScreenshotManager::captureSampleBufferWithFilter_configuration_completionHandler(
            filter,
            config,
            Some(&completion),
        );
    }
}

/// Finds `targets` in one ScreenCaptureKit listing and prepares a capture of each, handing `done` each
/// target with what was prepared for it, or `None` for a window the listing did not have.
pub fn list(
    targets: Vec<PictureTarget>,
    done: impl Fn(PictureTarget, Option<Prepared>) + Send + Sync + 'static,
) {
    shareable_content(DesktopWindows::Excluded, move |content| {
        let Some(content) = content else {
            debug!(
                count = targets.len(),
                "ScreenCaptureKit enumeration returned nothing"
            );
            for target in &targets {
                done(*target, None);
            }
            return;
        };
        // SAFETY: a property read of the live listing, which retains the array it returns.
        let windows = unsafe { content.windows() };
        for target in &targets {
            let found = windows.iter().find(|window| {
                // SAFETY: a property read of a window the live listing holds.
                (unsafe { window.windowID() }) == target.server_id.as_u32()
            });
            let Some(window) = found else {
                debug!(
                    wsid = target.server_id.as_u32(),
                    pid = target.window.pid,
                    "capture target not enumerated by ScreenCaptureKit"
                );
                done(*target, None);
                continue;
            };
            done(*target, Some(prepare(target, &window)));
        }
    });
}

fn prepare(target: &PictureTarget, window: &SCWindow) -> Prepared {
    // Buffer sized from the window as it is, not `target.size`: ScreenCaptureKit aspect-fits the real
    // window into whatever buffer it is given.
    // SAFETY: a property read of a window a live listing holds.
    let actual = unsafe { window.frame() }.size;
    let size = if actual.width >= 1.0 && actual.height >= 1.0 {
        actual
    } else {
        target.size.to_cg()
    };
    // SAFETY: an initialiser handed its own fresh allocation and a live window.
    let filter = unsafe {
        SCContentFilter::initWithDesktopIndependentWindow(SCContentFilter::alloc(), window)
    };
    // SAFETY: a new configuration, set up with plain values before anything else holds it.
    let config = unsafe {
        let config = SCStreamConfiguration::new();
        config.setWidth(((size.width * WINDOW_SCALE) as usize).max(1));
        config.setHeight(((size.height * WINDOW_SCALE) as usize).max(1));
        config.setPixelFormat(u32::from_be_bytes(*b"BGRA"));
        config.setShowsCursor(false);
        config.setCapturesAudio(false);
        // The compositor draws the tile's shadow; a baked one would double it.
        config.setIgnoreShadowsSingleWindow(true);
        config.setIgnoreGlobalClipSingleWindow(true);
        // Rounded corners must stay transparent.
        config.setShouldBeOpaque(false);
        // Nominal renders at point size into a pixel-sized buffer. See "Nominal capture resolution
        // paints a quarter of the buffer" in src/pictures/docs/capture-research.md.
        config.setCaptureResolution(SCCaptureResolutionType::Best);
        config
    };
    Prepared { size, filter, config }
}

/// Captures a window a listing found, handing `done` its picture, or `None` when the capture produced
/// no surface or did not fill its buffer.
pub fn capture(
    target: PictureTarget,
    prepared: Prepared,
    done: impl Fn(Option<Picture>) + Send + Sync + 'static,
) {
    let size = prepared.size;
    // Stamped here, not on delivery: the pixels are no older than this, and a capture coalesced into
    // one already running keeps that one's older stamp, so a caller can tell the two apart.
    let taken = std::time::Instant::now();
    screenshot(&prepared.filter, &prepared.config, move |buffer| {
        let filled = buffer.as_ref().and_then(|b| content_reaches_edges(b));
        let surface = buffer.and_then(|b| CVPixelBufferGetIOSurface(Some(&b)));
        // No capture calls in here: `CGWindowListCreateImage` is proxied through this same delivery
        // queue and deadlocks until a ~20s timeout, and `SLSCaptureWindowsContentsToRectWithOptions`
        // reaches the same proxy (`docs/data/2026-10-08-skylight-lock/callgraph.txt`).
        done(window_picture(&target, size, surface, filled, taken));
    });
}

fn window_picture(
    target: &PictureTarget,
    size: CGSize,
    surface: Option<CFRetained<IOSurfaceRef>>,
    filled: Option<bool>,
    taken: std::time::Instant,
) -> Option<Picture> {
    if filled == Some(false) {
        // Rejected rather than cached: an underfilled capture's coverage would claim the full buffer
        // and pass every fit test.
        warn!(
            wsid = target.server_id.as_u32(),
            pid = target.window.pid,
            "capture did not fill its buffer; rejected. A persistent repeat means the buffer is \
             sized wrongly (captureResolution, or points given as pixels)"
        );
        return None;
    }
    let Some(surface) = surface.as_deref().and_then(own_copy) else {
        debug!(
            wsid = target.server_id.as_u32(),
            pid = target.window.pid,
            "capture produced no surface"
        );
        return None;
    };
    Some(Picture {
        coverage: Coverage {
            covered: ScreenSize::new(
                surface.width() as f64 / WINDOW_SCALE,
                surface.height() as f64 / WINDOW_SCALE,
            ),
            window: ScreenSize::from_cg(size),
        },
        image: Pixels::Surface(surface),
        source: Route::ScreenCaptureKit,
        // The window on its own: grey wherever it has a blur. See `translucency`.
        carries_blur: false,
        taken,
        ground: LazyGround::default(),
    })
}

/// Renders `display` with every app window excluded, handing `done` the render. See "The wallpaper is
/// not reliably a window" in `src/pictures/docs/capture-research.md`.
pub fn render_desktop(display: Display, done: impl Fn(Option<Picture>) + Send + Sync + 'static) {
    let size = display.frame.size;
    let scale = display.scale;
    let done = Arc::new(done);
    shareable_content(DesktopWindows::Included, move |content| {
        let Some(content) = content else {
            debug!("ScreenCaptureKit enumeration returned nothing for the desktop");
            done(None);
            return;
        };
        // SAFETY: a property read of the live listing, which retains the array it returns.
        let displays = unsafe { content.displays() };
        let found = displays.iter().find(|found| {
            // SAFETY: a property read of a display the live listing holds.
            (unsafe { found.displayID() }) == display.id
        });
        let Some(found) = found else {
            let display_id = display.id;
            debug!(display_id, "display not enumerated by ScreenCaptureKit");
            done(None);
            return;
        };

        // The bar is excluded too, although below layer 0: the overlay draws it from its own capture.
        // See "The bar has to be captured on its own" in src/pictures/docs/capture-research.md.
        // SAFETY: a property read of the live listing, which retains the array it returns.
        let windows = unsafe { content.windows() };
        let excluded: Vec<Retained<SCWindow>> = windows
            .iter()
            .filter(|window| {
                // SAFETY: a property read of a window the live listing holds.
                let layer = unsafe { window.windowLayer() } as i64;
                layer >= 0 || crate::pictures::domain::backdrop::is_bar_layer(layer)
            })
            .collect();
        let excluded_refs: Vec<&SCWindow> = excluded.iter().map(|window| &**window).collect();
        let excluded = NSArray::from_slice(&excluded_refs);

        // SAFETY: an initialiser handed its own fresh allocation, a live display and an array of
        // live windows.
        let filter = unsafe {
            SCContentFilter::initWithDisplay_excludingWindows(
                SCContentFilter::alloc(),
                &found,
                &excluded,
            )
        };
        // SAFETY: a new configuration, set up with plain values before anything else holds it.
        let config = unsafe {
            let config = SCStreamConfiguration::new();
            config.setWidth(((size.width * scale) as usize).max(1));
            config.setHeight(((size.height * scale) as usize).max(1));
            config.setPixelFormat(u32::from_be_bytes(*b"BGRA"));
            config.setShowsCursor(false);
            config.setCapturesAudio(false);
            config.setShouldBeOpaque(true);
            config.setCaptureResolution(SCCaptureResolutionType::Nominal);
            config
        };

        let done = done.clone();
        screenshot(&filter, &config, move |buffer| {
            let surface = buffer.and_then(|buffer| CVPixelBufferGetIOSurface(Some(&buffer)));
            done(surface.as_deref().and_then(own_copy).map(|surface| Picture {
                coverage: Coverage {
                    covered: ScreenSize::new(
                        surface.width() as f64 / scale,
                        surface.height() as f64 / scale,
                    ),
                    window: size,
                },
                image: Pixels::Surface(surface),
                source: Route::ScreenCaptureKit,
                taken: std::time::Instant::now(),
                carries_blur: false,
                ground: LazyGround::default(),
            }));
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_captures_copy_is_its_own_surface_with_the_same_pixels() {
        let byte = |y: usize, x: usize| (y * 31 + x) as u8;
        let layout = Layout {
            bytes_per_row: None,
            pixel_format: Some(u32::from_be_bytes(*b"BGRA")),
        };
        let source = new_surface(7, 5, layout, |bytes, stride| {
            for y in 0..5 {
                for x in 0..7 * 4 {
                    bytes[y * stride + x] = byte(y, x);
                }
            }
            Some(())
        })
        .expect("a source surface");

        let copy = own_copy(&source).expect("a copy");
        assert_ne!(copy.id(), source.id(), "a surface of our own");
        assert_eq!((copy.width(), copy.height()), (7, 5));
        assert_eq!(copy.pixel_format(), source.pixel_format());
        let copy_stride = copy.bytes_per_row();
        read_locked(&copy, |bytes| {
            for y in 0..5 {
                for x in 0..7 * 4 {
                    assert_eq!(bytes[y * copy_stride + x], byte(y, x), "({x},{y})");
                }
            }
        })
        .expect("the copy reads");
    }

    fn target() -> PictureTarget {
        PictureTarget {
            window: rini_core::ids::WindowId::new(1, 1),
            server_id: rini_core::ids::WindowServerId::new(1),
            size: ScreenSize::new(859.0, 1081.0),
        }
    }

    /// A capture that produced nothing, or did not fill its buffer, yields no picture to deliver.
    #[test]
    fn a_capture_with_no_surface_or_an_underfilled_one_is_no_picture() {
        let size = CGSize::new(859.0, 1081.0);
        let now = std::time::Instant::now();
        assert!(window_picture(&target(), size, None, None, now).is_none());
        assert!(window_picture(&target(), size, None, Some(false), now).is_none());
    }

    /// Alpha reader over a plain buffer whose top-left `fill_w` x `fill_h` pixels are painted.
    /// Not a real IOSurface: creating one from a test thread races SkyLight's lazy initialisation.
    fn painted(fill_w: usize, fill_h: usize) -> impl Fn(usize, usize) -> u8 {
        move |x, y| if x < fill_w && y < fill_h { 255 } else { 0 }
    }

    #[test]
    fn a_capture_filling_only_a_corner_of_its_buffer_is_detected() {
        assert!(!edges_are_painted(64, 64, painted(32, 32)));
    }

    #[test]
    fn a_capture_filling_its_buffer_passes() {
        assert!(edges_are_painted(64, 64, painted(64, 64)));
    }

    #[test]
    fn a_capture_a_few_pixels_short_still_passes() {
        assert!(edges_are_painted(64, 64, painted(62, 62)));
    }

    #[test]
    fn a_capture_short_in_only_one_direction_is_detected() {
        assert!(!edges_are_painted(64, 64, painted(64, 32)));
        assert!(!edges_are_painted(64, 64, painted(32, 64)));
    }

    #[test]
    fn a_fully_transparent_capture_is_detected() {
        assert!(!edges_are_painted(64, 64, painted(0, 0)));
    }

    #[test]
    fn a_zero_sized_buffer_is_not_painted() {
        assert!(!edges_are_painted(0, 0, painted(0, 0)));
    }
}
