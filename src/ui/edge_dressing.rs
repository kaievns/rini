//! The macOS window hairline, harvested from a framed capture and worn by tiles.
//!
//! No surface capture carries the hairline the window server composites around every window: it is
//! drawn outside the app's surface, like the shadow. Unlike the shadow it cannot be synthesised
//! either, because its value depends on the window's own edge pixels and translucency — measured
//! constants matched an opaque window exactly and missed a translucent terminal by 2x. So the real
//! composited pixels are harvested from the one API that composites framing into its output, and a
//! tile wears them as thin sublayers. Measurements in `docs/capture-overlay-research.md`, "The
//! hairline is composited outside every capture".

use objc2_core_foundation::{CFRetained, CGPoint, CGRect, CGSize};
use objc2_core_graphics::{
    CGBitmapContextCreateImage, CGBitmapContextGetBytesPerRow, CGBitmapContextGetData,
    CGColorSpace, CGContext, CGImage, CGImageAlphaInfo, CGWindowListOption,
};

use crate::sys::window_server::{self, WindowServerId};

/// Corner radius of a macOS window, measured from where a capture's own alpha starts along its top row:
/// transparent for the first 18px to 20px at 2x backing scale.
pub const CORNER_RADIUS: f64 = 10.0;

/// The corner radius to draw a tile's silhouette with.
///
/// Clamped to half the shorter side. A rounded rect cannot have corners larger than that, and Core Graphics
/// clamps silently, so a small tile would otherwise get a silhouette nobody chose.
pub fn tile_corner_radius(size: CGSize) -> f64 {
    let shorter = size.width.min(size.height);
    CORNER_RADIUS.min(shorter / 2.0).max(0.0)
}

/// The hairline's thickness in points. Measured: 2 device pixels at 2x backing scale, on every edge.
pub const RING_PT: f64 = 1.0;

/// How opaque the harvested ring has to be, on average, to be believed.
///
/// A window that was not actually composited when the framed capture ran comes back fully
/// transparent, and a border tool's overlay window is transparent almost everywhere. Half is far
/// below any real window edge (the most translucent one measured, a see-through terminal, still
/// reads 244 of 255) and far above garbage.
const MIN_MEAN_ALPHA: f64 = 0.5;

/// Where the dressing sits on a window of `size`: four straight runs between the corners, and the
/// four corner boxes where the hairline curves.
///
/// One function for both uses — cropping the harvest in pixels and placing the sublayers in points —
/// so the two can never disagree about what goes where. Rects can be empty when the window is small;
/// callers skip those.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DressingLayout {
    /// Top, bottom, left, right, in that order.
    pub strips: [CGRect; 4],
    /// Top-left, top-right, bottom-left, bottom-right, in that order.
    pub corners: [CGRect; 4],
}

impl DressingLayout {
    fn offset(mut self, dx: f64, dy: f64) -> Self {
        for rect in self.strips.iter_mut().chain(self.corners.iter_mut()) {
            rect.origin.x += dx;
            rect.origin.y += dy;
        }
        self
    }
}

/// The dressing a tile wears, in window coordinates: a band straddling the window's boundary,
/// one ring inside it and one ring outside.
///
/// Both sides, because macOS frames a window with two lines: the light hairline on the window's
/// outermost point, and a near-black outline just OUTSIDE the bounds (measured at alpha 0.95),
/// which is what separates the border from the shadow. A tile wearing only the inner hairline had
/// the shadow bleeding straight into it, so the border read darker mid-flight and snapped at the
/// handover.
pub fn boundary_layout(size: CGSize) -> Option<DressingLayout> {
    let expanded = CGSize::new(size.width + 2.0 * RING_PT, size.height + 2.0 * RING_PT);
    let layout = dressing_layout(expanded, 2.0 * RING_PT, CORNER_RADIUS + RING_PT)?;
    Some(layout.offset(-RING_PT, -RING_PT))
}

pub fn dressing_layout(size: CGSize, ring: f64, corner: f64) -> Option<DressingLayout> {
    let (w, h) = (size.width, size.height);
    if w <= 0.0 || h <= 0.0 || ring <= 0.0 {
        return None;
    }
    // The corner boxes cannot outgrow the window, and must hold at least the ring itself.
    let c = corner.min(w / 2.0).min(h / 2.0).max(ring.min(w / 2.0).min(h / 2.0));
    let r = ring.min(c);
    let run_w = (w - 2.0 * c).max(0.0);
    let run_h = (h - 2.0 * c).max(0.0);
    Some(DressingLayout {
        strips: [
            CGRect::new(CGPoint::new(c, 0.0), CGSize::new(run_w, r)),
            CGRect::new(CGPoint::new(c, h - r), CGSize::new(run_w, r)),
            CGRect::new(CGPoint::new(0.0, c), CGSize::new(r, run_h)),
            CGRect::new(CGPoint::new(w - r, c), CGSize::new(r, run_h)),
        ],
        corners: [
            CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(c, c)),
            CGRect::new(CGPoint::new(w - c, 0.0), CGSize::new(c, c)),
            CGRect::new(CGPoint::new(0.0, h - c), CGSize::new(c, c)),
            CGRect::new(CGPoint::new(w - c, h - c), CGSize::new(c, c)),
        ],
    })
}

/// Is a harvested ring real pixels rather than a window that was never composited?
pub fn dressing_is_real(mean_alpha: f64) -> bool {
    mean_alpha >= MIN_MEAN_ALPHA
}

/// The side of the square a [`thumbprint`] samples an image down to.
const THUMBPRINT_SIDE: usize = 32;

/// How different two consecutive thumbprints may be and still count as the same rendering.
///
/// Zero would never settle: a terminal's cursor blinks, a clock ticks. Three percent of samples
/// forgives those while a half-painted surface — the reveal chase's actual enemy, where whole
/// regions fill in between captures — differs across a large share of the image.
const STABLE_MAX_DIFFERING: f64 = 0.03;
const STABLE_CHANNEL_TOLERANCE: u8 = 8;

/// A small fingerprint of an image's content: the image drawn down to a 32x32 RGBA square.
///
/// Exists for the reveal chase, which must not deliver a capture of a window whose app has not
/// finished PAINTING at its new size — the frame resizes instantly, the pixels lag — so it
/// compares consecutive fingerprints until the rendering stops changing.
pub fn thumbprint(image: &CGImage) -> Option<Vec<u8>> {
    let side = THUMBPRINT_SIDE;
    let space = CGColorSpace::new_device_rgb()?;
    // SAFETY: a fresh context; CG owns and frees the backing store with it.
    let ctx = unsafe {
        CGBitmapContextCreate(
            std::ptr::null_mut(),
            side,
            side,
            8,
            0,
            Some(&space),
            CGImageAlphaInfo::PremultipliedLast.0,
        )
    };
    let ctx = unsafe { CFRetained::from_raw(std::ptr::NonNull::new(ctx)?) };
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
        // SAFETY: rows within the context's own buffer.
        out.extend_from_slice(unsafe { std::slice::from_raw_parts(data.add(y * stride), side * 4) });
    }
    Some(out)
}

/// Whether two consecutive thumbprints show the same rendering.
pub fn renderings_match(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() || a.is_empty() {
        return false;
    }
    let differing = a
        .iter()
        .zip(b)
        .filter(|(x, y)| x.abs_diff(**y) > STABLE_CHANNEL_TOLERANCE)
        .count();
    (differing as f64) <= (a.len() as f64) * STABLE_MAX_DIFFERING
}

/// The harvested hairline: small owned bitmaps, in [`DressingLayout`] order.
///
/// Owned copies rather than crops of the framed capture, because a crop keeps the whole
/// window-sized parent image alive: 28MB per window against ~200KB for the ring.
#[derive(Clone, Debug)]
pub struct EdgeDressing {
    pub strips: [Option<CFRetained<CGImage>>; 4],
    pub corners: [Option<CFRetained<CGImage>>; 4],
}

// CGImage is immutable, and the dressing crosses from capture threads to the main thread the same
// way `SnapshotImage::Bitmap` does.
unsafe impl Send for EdgeDressing {}

/// Which dressing the cache holds after a capture lands: a fresh harvest wins, and a capture that
/// harvested nothing keeps whatever the window last wore.
///
/// Applies whether or not the capture's PIXELS were accepted: the harvest is taken from the screen
/// composite, not from the capture buffer, so a clipped capture can still carry a good ring.
pub fn dressing_after_insert<D>(worn: Option<D>, harvested: Option<D>) -> Option<D> {
    harvested.or(worn)
}

/// Harvests the window's composited edge off the screen: a framed capture, cropped to the ring.
///
/// `CGWindowListCreateImage` is the one capture API that composites window-server framing — the
/// hairline occupies the window rect's outermost point on every edge — and it only renders windows
/// that are actually composited, so a parked window comes back transparent and is rejected by the
/// alpha check. Deprecated in favour of ScreenCaptureKit, which is exactly the API that CANNOT do
/// this; the symbol is alive and measured at 16-24ms, so it stays off the hot paths.
pub fn harvest_edge_dressing(server_id: WindowServerId, scale: f64) -> Option<EdgeDressing> {
    let frame = window_server::get_window(server_id)?.frame;
    if frame.size.width <= 0.0 || frame.size.height <= 0.0 || scale <= 0.0 {
        return None;
    }
    // One ring beyond the bounds, so the capture carries the outer dark outline too (see
    // [`boundary_layout`]). The margin holds the first ring of shadow as well, which the crops
    // deliberately keep: it is what that pixel really looks like on screen.
    let expanded = CGRect::new(
        CGPoint::new(frame.origin.x - RING_PT, frame.origin.y - RING_PT),
        CGSize::new(frame.size.width + 2.0 * RING_PT, frame.size.height + 2.0 * RING_PT),
    );
    #[allow(deprecated)]
    let framed = objc2_core_graphics::CGWindowListCreateImage(
        expanded,
        CGWindowListOption::OptionIncludingWindow,
        server_id.as_u32(),
        objc2_core_graphics::CGWindowImageOption::empty(),
    )?;
    let px_w = CGImage::width(Some(&framed)) as f64;
    let px_h = CGImage::height(Some(&framed)) as f64;
    // A capture of another size does not line up with the window rect, so the crops would be lies.
    if (px_w - expanded.size.width * scale).abs() > 2.0
        || (px_h - expanded.size.height * scale).abs() > 2.0
    {
        return None;
    }
    // The clip hugs the dark outline's own rounded arc, one ring outside the window's corner.
    let radius_px = (tile_corner_radius(frame.size) + RING_PT) * scale;
    let layout = boundary_layout(frame.size)?;
    // Placement rects are in window coordinates (origin can be one ring negative); the capture's
    // origin is one ring before the window's, so crops shift by exactly that ring.
    let crop = |rect: CGRect| {
        CGRect::new(
            CGPoint::new((rect.origin.x + RING_PT) * scale, (rect.origin.y + RING_PT) * scale),
            CGSize::new(rect.size.width * scale, rect.size.height * scale),
        )
    };

    let mut alpha_sum = 0.0;
    let mut alpha_samples = 0usize;
    let mut strips: [Option<CFRetained<CGImage>>; 4] = [None, None, None, None];
    for (slot, region) in strips.iter_mut().zip(layout.strips) {
        let Some((image, alpha, samples)) = copy_region(&framed, px_w, px_h, crop(region), None)
        else {
            continue;
        };
        alpha_sum += alpha;
        alpha_samples += samples;
        *slot = Some(image);
    }
    // Judged on the straight runs alone: corner boxes are legitimately transparent outside the arc.
    if alpha_samples == 0 || !dressing_is_real(alpha_sum / alpha_samples as f64) {
        return None;
    }
    let mut corners: [Option<CFRetained<CGImage>>; 4] = [None, None, None, None];
    for (slot, region) in corners.iter_mut().zip(layout.corners) {
        *slot = copy_region(&framed, px_w, px_h, crop(region), Some(radius_px))
            .map(|(image, ..)| image);
    }
    Some(EdgeDressing { strips, corners })
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

/// Copies `region` (top-left pixel coordinates) of `image` into a small owned image, reporting the
/// mean alpha of what was copied. `rounded` clips the draw to the window's rounded-rect silhouette,
/// which is how a corner box keeps the curving hairline but drops the shadow outside the arc.
fn copy_region(
    image: &CGImage,
    img_w: f64,
    img_h: f64,
    region: CGRect,
    rounded: Option<f64>,
) -> Option<(CFRetained<CGImage>, f64, usize)> {
    let w = region.size.width.round() as usize;
    let h = region.size.height.round() as usize;
    if w == 0 || h == 0 {
        return None;
    }
    let space = CGColorSpace::new_device_rgb()?;
    // SAFETY: a fresh context; CG owns and frees the backing store with it.
    let ctx = unsafe {
        CGBitmapContextCreate(
            std::ptr::null_mut(),
            w,
            h,
            8,
            0,
            Some(&space),
            CGImageAlphaInfo::PremultipliedLast.0,
        )
    };
    let ctx = unsafe { CFRetained::from_raw(std::ptr::NonNull::new(ctx)?) };
    // The context's origin is bottom-left; the region is measured from the top. Draw the whole
    // image offset so the region's pixels land on the context.
    let origin = CGPoint::new(-region.origin.x, -(img_h - region.origin.y - region.size.height));
    if let Some(radius) = rounded {
        let silhouette = CGRect::new(origin, CGSize::new(img_w, img_h));
        // SAFETY: a null transform means the path is taken as given.
        let path = unsafe {
            objc2_core_graphics::CGPath::with_rounded_rect(
                silhouette,
                radius,
                radius,
                std::ptr::null(),
            )
        };
        CGContext::add_path(Some(&ctx), Some(&path));
        CGContext::clip(Some(&ctx));
    }
    CGContext::draw_image(
        Some(&ctx),
        CGRect::new(origin, CGSize::new(img_w, img_h)),
        Some(image),
    );

    // Premultiplied-last RGBA: the alpha byte is every fourth, offset three.
    let data = CGBitmapContextGetData(Some(&ctx)) as *const u8;
    if data.is_null() {
        return None;
    }
    let stride = CGBitmapContextGetBytesPerRow(Some(&ctx));
    let mut sum = 0.0f64;
    let mut samples = 0usize;
    for y in (0..h).step_by((h / 8).max(1)) {
        for x in (0..w).step_by((w / 8).max(1)) {
            // SAFETY: x < w and y < h index inside the context's own buffer.
            sum += unsafe { *data.add(y * stride + x * 4 + 3) } as f64 / 255.0;
            samples += 1;
        }
    }
    let copied = CGBitmapContextCreateImage(Some(&ctx))?;
    Some((copied, sum, samples))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: f64, y: f64, w: f64, h: f64) -> CGRect {
        CGRect::new(CGPoint::new(x, y), CGSize::new(w, h))
    }

    /// The tile's silhouette has to match the window's own rounded corners, or the shadow shows
    /// through the transparent corner as a hard square.
    #[test]
    fn a_normal_window_gets_the_measured_corner_radius() {
        assert_eq!(tile_corner_radius(CGSize::new(859.0, 1081.0)), CORNER_RADIUS);
        assert_eq!(tile_corner_radius(CGSize::new(1720.0, 1081.0)), CORNER_RADIUS);
    }

    #[test]
    fn a_tile_thinner_than_the_radius_is_clamped_to_half_its_shorter_side() {
        // Core Graphics clamps this silently, so doing it here keeps the silhouette predictable.
        assert_eq!(tile_corner_radius(CGSize::new(12.0, 400.0)), 6.0);
        assert_eq!(tile_corner_radius(CGSize::new(400.0, 8.0)), 4.0);
    }

    #[test]
    fn a_zero_sized_tile_has_no_corners_rather_than_negative_ones() {
        assert_eq!(tile_corner_radius(CGSize::new(0.0, 0.0)), 0.0);
        assert_eq!(tile_corner_radius(CGSize::new(-10.0, 100.0)), 0.0);
    }

    #[test]
    fn layout_covers_the_perimeter_without_overlap() {
        // A 100x60 window with 10pt corners and a 1pt ring: runs meet the corner boxes exactly.
        let layout = dressing_layout(CGSize::new(100.0, 60.0), 1.0, 10.0).unwrap();
        assert_eq!(layout.strips[0], rect(10.0, 0.0, 80.0, 1.0), "top run between the corners");
        assert_eq!(layout.strips[1], rect(10.0, 59.0, 80.0, 1.0), "bottom run");
        assert_eq!(layout.strips[2], rect(0.0, 10.0, 1.0, 40.0), "left run");
        assert_eq!(layout.strips[3], rect(99.0, 10.0, 1.0, 40.0), "right run");
        assert_eq!(layout.corners[0], rect(0.0, 0.0, 10.0, 10.0));
        assert_eq!(layout.corners[1], rect(90.0, 0.0, 10.0, 10.0));
        assert_eq!(layout.corners[2], rect(0.0, 50.0, 10.0, 10.0));
        assert_eq!(layout.corners[3], rect(90.0, 50.0, 10.0, 10.0));
    }

    #[test]
    fn layout_scales_with_backing_scale() {
        // The same window in 2x pixels: everything doubles, so crops and placement line up.
        let pt = dressing_layout(CGSize::new(100.0, 60.0), 1.0, 10.0).unwrap();
        let px = dressing_layout(CGSize::new(200.0, 120.0), 2.0, 20.0).unwrap();
        for (a, b) in pt.strips.iter().zip(px.strips) {
            assert_eq!(a.origin.x * 2.0, b.origin.x);
            assert_eq!(a.size.width * 2.0, b.size.width);
        }
    }

    #[test]
    fn corners_swallow_the_whole_edge_of_a_window_smaller_than_two_corners() {
        // A 16x16 window cannot hold two 10pt corner boxes plus a run: corners clamp to half the
        // side and the runs collapse to empty, which callers skip.
        let layout = dressing_layout(CGSize::new(16.0, 16.0), 1.0, 10.0).unwrap();
        assert_eq!(layout.strips[0].size.width, 0.0);
        assert_eq!(layout.corners[0], rect(0.0, 0.0, 8.0, 8.0));
        assert_eq!(layout.corners[3], rect(8.0, 8.0, 8.0, 8.0));
    }

    /// The band straddles the boundary: one ring outside the window (the dark outline lives
    /// there), one ring inside (the light hairline). Origins go negative by exactly one ring.
    #[test]
    fn the_boundary_band_straddles_the_window_edge() {
        let layout = boundary_layout(CGSize::new(100.0, 60.0)).unwrap();
        // Corner boxes grow by the ring to keep hugging the arc, so runs start one ring earlier.
        assert_eq!(layout.strips[0], rect(10.0, -1.0, 80.0, 2.0), "top: 1pt out to 1pt in");
        assert_eq!(layout.strips[1], rect(10.0, 59.0, 80.0, 2.0), "bottom");
        assert_eq!(layout.strips[2], rect(-1.0, 10.0, 2.0, 40.0), "left");
        assert_eq!(layout.strips[3], rect(99.0, 10.0, 2.0, 40.0), "right");
        assert_eq!(layout.corners[0], rect(-1.0, -1.0, 11.0, 11.0));
        assert_eq!(layout.corners[3], rect(90.0, 50.0, 11.0, 11.0), "10pt inside to 1pt outside");
    }

    #[test]
    fn a_degenerate_window_has_no_layout() {
        assert_eq!(dressing_layout(CGSize::new(0.0, 60.0), 1.0, 10.0), None);
        assert_eq!(dressing_layout(CGSize::new(100.0, 60.0), 0.0, 10.0), None);
    }

    #[test]
    fn a_transparent_harvest_is_not_believed() {
        // What a parked window's framed capture measures: nothing was composited.
        assert!(!dressing_is_real(0.0));
        assert!(!dressing_is_real(0.3));
    }

    #[test]
    fn a_translucent_terminal_edge_is_still_real() {
        // The most translucent real edge measured: alpha 244 of 255.
        assert!(dressing_is_real(244.0 / 255.0));
        assert!(dressing_is_real(1.0));
    }

    /// Blinking cursors and ticking clocks must not keep the reveal chase waiting forever; a
    /// half-painted surface filling in — whole regions changing between captures — must.
    #[test]
    fn a_rendering_matches_itself_through_cursor_noise_but_not_through_repaints() {
        let a = vec![100u8; 4096];
        assert!(renderings_match(&a, &a), "identical");
        let mut cursor = a.clone();
        for value in cursor.iter_mut().take(80) {
            *value = 200; // ~2% of samples: a cursor cell, a clock digit
        }
        assert!(renderings_match(&a, &cursor), "cursor-sized noise still matches");
        let mut repaint = a.clone();
        for value in repaint.iter_mut().take(1024) {
            *value = 200; // a quarter of the image filled in
        }
        assert!(!renderings_match(&a, &repaint), "a repaint does not");
    }

    #[test]
    fn mismatched_or_empty_thumbprints_never_match() {
        assert!(!renderings_match(&[1, 2, 3], &[1, 2]));
        assert!(!renderings_match(&[], &[]));
    }

    #[test]
    fn a_fresh_harvest_replaces_what_the_window_wore() {
        assert_eq!(dressing_after_insert(Some(1), Some(2)), Some(2));
    }

    #[test]
    fn a_failed_harvest_keeps_the_last_known_dressing() {
        // A window captured while parked harvests nothing; it keeps the ring from when it was last
        // on screen, the same staleness model as the picture cache itself.
        assert_eq!(dressing_after_insert(Some(1), None), Some(1));
        assert_eq!(dressing_after_insert::<u32>(None, None), None);
    }
}
