//! Pictures read on the service's thread: a picture's band print and its body samples, which say
//! whether it shows anything new (`crate::pictures::domain::print`), and what a window's first picture
//! says of it (`crate::pictures::domain::knowledge`), each drawing only as much of the picture as it
//! needs; and the overlay and the screen drawn whole for a handover check
//! (`crate::pictures::domain::handover`).

use objc2_core_foundation::{CFRetained, CGPoint, CGRect, CGSize};
use objc2_core_graphics::{
    CGBitmapContextGetBytesPerRow, CGBitmapContextGetData, CGContext, CGImage,
    CGInterpolationQuality,
};
use objc2_io_surface::{IOSurfaceLockOptions, IOSurfaceRef};
use rini_geometry::space::ScreenPoint;

use crate::pictures::domain::handover::Rgba;
use crate::pictures::domain::knowledge::{
    CORNER_SIDE_PT, MATERIAL_SIDE, TRANSLUCENCY_SIDE, corner_radius, has_material, is_translucent,
};
use crate::pictures::domain::print::{
    BAND_PRINT, BAND_PT, BODY_ACROSS, BODY_DOWN, BODY_GRID, body_points,
};
use crate::pictures::platform::bitmap::rgba_bitmap_context;
use crate::pictures::platform::picture::{Picture, Pixels, image_size};

/// The band print of a picture: its top `BAND_PT` at the picture's own scale, drawn down to
/// `BAND_PRINT`. `None` for a surface.
pub fn band_print(picture: &Picture) -> Option<Vec<u8>> {
    let image = picture.image.bitmap()?;
    let pixels = image_size(&image);
    let (width, height) = (pixels.width, pixels.height);
    let scale = height / picture.coverage.covered.height.max(1.0);
    let band = CGRect::new(
        CGPoint::new(0.0, 0.0),
        CGSize::new(width, (BAND_PT * scale).round().min(height)),
    );
    let band = CGImage::with_image_in_rect(Some(&image), band)?;
    let (bytes, _) = drawn(
        &band,
        BAND_PRINT.0,
        BAND_PRINT.1,
        CGInterpolationQuality::Default,
    )?;
    Some(bytes)
}

/// A picture's body sampled at [`body_points`], as RGB, for `bodies_differ`.
pub fn body_samples(picture: &Picture) -> Option<Vec<[u8; 3]>> {
    match &picture.image {
        Pixels::Surface(surface) => surface_samples(surface),
        Pixels::Bitmap(_) | Pixels::Region { .. } => {
            let image = picture.image.bitmap()?;
            let pixels = image_size(&image);
            let (width, height) = (pixels.width, pixels.height);
            let body = CGRect::new(
                CGPoint::new(BODY_ACROSS.0 * width, BODY_DOWN.0 * height),
                CGSize::new(
                    (BODY_ACROSS.1 - BODY_ACROSS.0) * width,
                    (BODY_DOWN.1 - BODY_DOWN.0) * height,
                ),
            );
            let body = CGImage::with_image_in_rect(Some(&image), body)?;
            // One source pixel per cell, at its centre: the grid's points, not an average of them.
            let (bytes, stride) = drawn(&body, BODY_GRID, BODY_GRID, CGInterpolationQuality::None)?;
            Some(
                (0..BODY_GRID * BODY_GRID)
                    .map(|i| {
                        let at = (i / BODY_GRID) * stride + (i % BODY_GRID) * 4;
                        [bytes[at], bytes[at + 1], bytes[at + 2]]
                    })
                    .collect(),
            )
        }
    }
}

/// A surface's pixels at the body's points, BGRA read as RGB.
fn surface_samples(surface: &IOSurfaceRef) -> Option<Vec<[u8; 3]>> {
    let (width, height, stride) = (surface.width(), surface.height(), surface.bytes_per_row());
    if width == 0 || height == 0 || stride < width * 4 {
        return None;
    }
    // SAFETY: locked for reading, and every read below is inside `stride * height`.
    unsafe {
        if surface.lock(IOSurfaceLockOptions::ReadOnly, std::ptr::null_mut()) != 0 {
            return None;
        }
        let base = surface.base_address().as_ptr() as *const u8;
        let samples = body_points()
            .map(|(u, v)| {
                let x = ((u * width as f64) as usize).min(width - 1);
                let y = ((v * height as f64) as usize).min(height - 1);
                let pixel = base.add(y * stride + x * 4);
                [*pixel.add(2), *pixel.add(1), *pixel]
            })
            .collect();
        surface.unlock(IOSurfaceLockOptions::ReadOnly, std::ptr::null_mut());
        Some(samples)
    }
}

/// What a window's first picture, `crop` at `scale`, says of it: whether it is translucent, from the
/// picture drawn small, and its corner radius, from its top-left corner alone.
pub fn first_sight(crop: &CGImage, scale: f64) -> (bool, Option<f64>) {
    let translucent = drawn(
        crop,
        TRANSLUCENCY_SIDE,
        TRANSLUCENCY_SIDE,
        CGInterpolationQuality::Default,
    )
    .is_some_and(|(bytes, stride)| is_translucent(&bytes, stride));
    let side = (CORNER_SIDE_PT * scale).round();
    let corner = CGImage::with_image_in_rect(
        Some(crop),
        CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(side, side)),
    );
    let radius = corner.and_then(|corner| {
        let (width, height) = (CGImage::width(Some(&corner)), CGImage::height(Some(&corner)));
        let (bytes, stride) = drawn(&corner, width, height, CGInterpolationQuality::None)?;
        corner_radius(width, height, scale, |x, y| bytes[y * stride + x * 4 + 3])
    });
    (translucent, radius)
}

/// Whether a window has a material: its picture `alone` and `baked` over what is behind it, both drawn
/// small, differ.
pub fn shows_material(alone: &CGImage, baked: &CGImage) -> bool {
    let draw = |image| {
        drawn(
            image,
            MATERIAL_SIDE,
            MATERIAL_SIDE,
            CGInterpolationQuality::Default,
        )
    };
    match (draw(alone), draw(baked)) {
        (Some((alone, stride)), Some((baked, _))) => has_material(&alone, &baked, stride),
        _ => false,
    }
}

/// A picture drawn whole into RGBA8, its bytes top row first.
pub struct Drawn {
    pub width: usize,
    pub height: usize,
    pub stride: usize,
    pub bytes: Vec<u8>,
}

impl Drawn {
    /// Its pixels as a handover check reads them: its top-left at `origin` on screen, `scale` pixels a
    /// point.
    pub fn at(&self, origin: ScreenPoint, scale: f64) -> Rgba<'_> {
        Rgba { width: self.width, height: self.height, stride: self.stride, bytes: &self.bytes, origin, scale }
    }
}

/// `image` drawn whole, pixel for pixel.
pub fn rgba(image: &CGImage) -> Option<Drawn> {
    let (width, height) = (CGImage::width(Some(image)), CGImage::height(Some(image)));
    let (bytes, stride) = drawn(image, width, height, CGInterpolationQuality::None)?;
    Some(Drawn { width, height, stride, bytes })
}

/// `image` drawn into a fresh `width` x `height` RGBA context, and its bytes top row first, with the
/// stride.
fn drawn(
    image: &CGImage,
    width: usize,
    height: usize,
    quality: CGInterpolationQuality,
) -> Option<(Vec<u8>, usize)> {
    let ctx: CFRetained<CGContext> = rgba_bitmap_context(width, height)?;
    CGContext::set_interpolation_quality(Some(&ctx), quality);
    CGContext::draw_image(
        Some(&ctx),
        CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(width as f64, height as f64)),
        Some(image),
    );
    let data = CGBitmapContextGetData(Some(&ctx)) as *const u8;
    if data.is_null() {
        return None;
    }
    let stride = CGBitmapContextGetBytesPerRow(Some(&ctx));
    // SAFETY: the context owns `stride * height` bytes and outlives this copy.
    Some((
        unsafe { std::slice::from_raw_parts(data, stride * height) }.to_vec(),
        stride,
    ))
}

#[cfg(test)]
mod tests {
    use objc2_core_graphics::CGBitmapContextCreateImage;
    use rini_geometry::space::{FromCg, PixelRect, ScreenSize};

    use super::*;
    use crate::pictures::domain::coverage::Coverage;
    use crate::pictures::domain::print::bodies_differ;
    use crate::pictures::platform::picture::test_picture;

    /// An image of `width` x `height` pixels, `paint` filling it in user space (origin bottom left).
    fn image(width: usize, height: usize, paint: impl Fn(&CGContext)) -> CFRetained<CGImage> {
        let ctx = rgba_bitmap_context(width, height).unwrap();
        paint(&ctx);
        CGBitmapContextCreateImage(Some(&ctx)).unwrap()
    }

    fn fill(ctx: &CGContext, rect: CGRect, rgba: (f64, f64, f64, f64)) {
        CGContext::set_rgb_fill_color(Some(ctx), rgba.0, rgba.1, rgba.2, rgba.3);
        CGContext::fill_rect(Some(ctx), rect);
    }

    fn rect(x: f64, y: f64, w: f64, h: f64) -> CGRect {
        CGRect::new(CGPoint::new(x, y), CGSize::new(w, h))
    }

    /// A window of `size` points at `scale`: grey, with a 20pt title band of `band` colour across its
    /// top, inside the 8% the body leaves out.
    fn window(size: (f64, f64), scale: f64, band: (f64, f64, f64)) -> Picture {
        let (w, h) = ((size.0 * scale) as usize, (size.1 * scale) as usize);
        let pixels = image(w, h, |ctx| {
            fill(ctx, rect(0.0, 0.0, w as f64, h as f64), (0.5, 0.5, 0.5, 1.0));
            let band_px = 20.0 * scale;
            fill(
                ctx,
                rect(0.0, h as f64 - band_px, w as f64, band_px),
                (band.0, band.1, band.2, 1.0),
            );
        });
        Picture {
            image: Pixels::Bitmap(pixels),
            coverage: Coverage {
                covered: ScreenSize::new(size.0, size.1),
                window: ScreenSize::new(size.0, size.1),
            },
            ..test_picture(ScreenSize::new(size.0, size.1))
        }
    }

    /// A focus change repaints the title band, and the band print sees it; a change below the band
    /// does not show in it.
    #[test]
    fn a_band_print_sees_the_title_band_and_nothing_below_it() {
        let focused = band_print(&window((400.0, 300.0), 2.0, (0.9, 0.9, 0.9))).unwrap();
        let unfocused = band_print(&window((400.0, 300.0), 2.0, (0.7, 0.7, 0.7))).unwrap();
        assert_eq!(focused.len(), BAND_PRINT.0 * BAND_PRINT.1 * 4);
        assert_ne!(focused, unfocused);

        let mut changed_below = window((400.0, 300.0), 2.0, (0.9, 0.9, 0.9));
        let Pixels::Bitmap(base) = &changed_below.image else {
            unreachable!()
        };
        let base = base.clone();
        changed_below.image = Pixels::Bitmap(image(800, 600, |ctx| {
            CGContext::draw_image(Some(ctx), rect(0.0, 0.0, 800.0, 600.0), Some(&base));
            fill(ctx, rect(0.0, 0.0, 800.0, 400.0), (0.1, 0.1, 0.1, 1.0));
        }));
        assert_eq!(band_print(&changed_below).unwrap(), focused);
    }

    /// The band is 40pt at the picture's own scale: at 1x it is 40 pixels, not 80, so a 1x picture
    /// prints what a 2x picture of the same window does.
    #[test]
    fn the_band_is_forty_points_at_the_pictures_own_scale() {
        let at_1x = band_print(&window((400.0, 300.0), 1.0, (0.9, 0.9, 0.9))).unwrap();
        let at_2x = band_print(&window((400.0, 300.0), 2.0, (0.9, 0.9, 0.9))).unwrap();
        let difference =
            at_1x.iter().zip(&at_2x).map(|(a, b)| a.abs_diff(*b) as u32).max().unwrap();
        assert!(difference <= 2, "{difference}");
    }

    #[test]
    fn a_surface_has_no_band_print() {
        let mut picture = test_picture(ScreenSize::new(1.0, 1.0));
        picture.image = Pixels::Surface(test_surface(4, 4, [0, 0, 0, 255]));
        assert!(band_print(&picture).is_none());
    }

    /// A region prints as its crop does.
    #[test]
    fn a_region_prints_as_its_crop() {
        let whole = window((400.0, 300.0), 2.0, (0.9, 0.9, 0.9));
        let Pixels::Bitmap(pixels) = &whole.image else {
            unreachable!()
        };
        let composite = image(1600, 600, |ctx| {
            CGContext::draw_image(Some(ctx), rect(800.0, 0.0, 800.0, 600.0), Some(pixels));
        });
        let region = Picture {
            image: Pixels::Region {
                composite,
                rect: PixelRect::from_cg(rect(800.0, 0.0, 800.0, 600.0)),
            },
            ..whole.clone()
        };
        assert_eq!(band_print(&region), band_print(&whole));
    }

    /// Bodies sampled from a bitmap and from a surface of the same pixels agree, and a repaint below
    /// the band shows.
    #[test]
    fn bodies_are_sampled_alike_from_any_kind_and_a_repaint_shows() {
        let grey = window((400.0, 300.0), 2.0, (0.9, 0.9, 0.9));
        let a = body_samples(&grey).unwrap();
        assert_eq!(a.len(), BODY_GRID * BODY_GRID);
        assert!(
            a.iter().all(|rgb| rgb.iter().all(|c| c.abs_diff(128) <= 1)),
            "grey body"
        );

        let mut surface = test_picture(ScreenSize::new(400.0, 300.0));
        surface.image = Pixels::Surface(test_surface(800, 600, [128, 128, 128, 255]));
        assert!(!bodies_differ(&a, &body_samples(&surface).unwrap()));

        surface.image = Pixels::Surface(test_surface(800, 600, [20, 20, 20, 255]));
        assert!(bodies_differ(&a, &body_samples(&surface).unwrap()));
        assert!(
            !bodies_differ(
                &a,
                &body_samples(&window((400.0, 300.0), 2.0, (0.1, 0.1, 0.1))).unwrap()
            ),
            "the band is not the body"
        );
    }

    /// First sight of an opaque window with rounded corners, and of a translucent one.
    #[test]
    fn first_sight_finds_translucency_and_the_corner_radius() {
        let (w, h) = (400usize, 300usize);
        let radius = 20.0;
        let rounded = image(w, h, |ctx| {
            let path = unsafe {
                objc2_core_graphics::CGPath::with_rounded_rect(
                    rect(0.0, 0.0, w as f64, h as f64),
                    radius,
                    radius,
                    std::ptr::null(),
                )
            };
            CGContext::add_path(Some(ctx), Some(&path));
            CGContext::set_rgb_fill_color(Some(ctx), 0.3, 0.3, 0.3, 1.0);
            CGContext::fill_path(Some(ctx));
        });
        let (translucent, measured) = first_sight(&rounded, 2.0);
        assert!(!translucent);
        let measured = measured.expect("a radius");
        assert!((measured - 10.0).abs() < 1.0, "20px at 2x is 10pt: {measured}");

        let glass = image(w, h, |ctx| {
            fill(ctx, rect(0.0, 0.0, w as f64, h as f64), (0.1, 0.1, 0.1, 0.6))
        });
        assert!(first_sight(&glass, 2.0).0, "translucent");
    }

    #[test]
    fn a_material_shows_as_the_baked_picture_differing_from_the_window_alone() {
        let alone = image(200, 200, |ctx| {
            fill(ctx, rect(0.0, 0.0, 200.0, 200.0), (0.5, 0.5, 0.5, 1.0))
        });
        let blurred = image(200, 200, |ctx| {
            fill(ctx, rect(0.0, 0.0, 200.0, 200.0), (0.5, 0.5, 0.5, 1.0));
            fill(ctx, rect(0.0, 0.0, 60.0, 200.0), (0.2, 0.4, 0.7, 1.0));
        });
        assert!(!shows_material(&alone, &alone));
        assert!(shows_material(&alone, &blurred));
    }

    /// Drawn whole, an image keeps every pixel where it is, the top row first: the handover check reads
    /// it at screen points.
    #[test]
    fn a_picture_drawn_whole_keeps_its_pixels_top_row_first() {
        let top_red = image(4, 2, |ctx| {
            fill(ctx, rect(0.0, 0.0, 4.0, 2.0), (0.0, 0.0, 1.0, 1.0));
            fill(ctx, rect(0.0, 1.0, 4.0, 1.0), (1.0, 0.0, 0.0, 1.0));
        });
        let drawn = rgba(&top_red).unwrap();
        assert_eq!((drawn.width, drawn.height), (4, 2));
        assert_eq!(&drawn.bytes[..4], &[255, 0, 0, 255], "the top row");
        assert_eq!(&drawn.bytes[drawn.stride..drawn.stride + 4], &[0, 0, 255, 255]);
        let read = drawn.at(ScreenPoint::new(10.0, 20.0), 2.0);
        assert_eq!((read.origin, read.scale, read.bytes.len()), (ScreenPoint::new(10.0, 20.0), 2.0, drawn.stride * 2));
    }

    fn test_surface(width: i64, height: i64, bgra_of_rgba: [u8; 4]) -> CFRetained<IOSurfaceRef> {
        use objc2_core_foundation::{CFDictionary, CFNumber, CFString};
        use objc2_io_surface::{kIOSurfaceBytesPerElement, kIOSurfaceHeight, kIOSurfaceWidth};
        let keys: [&CFString; 3] =
            unsafe { [kIOSurfaceWidth, kIOSurfaceHeight, kIOSurfaceBytesPerElement] };
        let values = [
            CFNumber::new_i64(width),
            CFNumber::new_i64(height),
            CFNumber::new_i64(4),
        ];
        let value_refs: [&CFNumber; 3] = std::array::from_fn(|i| &*values[i]);
        // SAFETY: the three keys an IOSurface needs, each with a number.
        let surface =
            unsafe { IOSurfaceRef::new(CFDictionary::from_slices(&keys, &value_refs).as_opaque()) }
                .expect("a surface");
        let [r, g, b, a] = bgra_of_rgba;
        // SAFETY: locked for writing, within `stride * height`.
        unsafe {
            assert_eq!(
                surface.lock(IOSurfaceLockOptions::empty(), std::ptr::null_mut()),
                0
            );
            let base = surface.base_address().as_ptr() as *mut u8;
            let stride = surface.bytes_per_row();
            for y in 0..height as usize {
                for x in 0..width as usize {
                    let pixel = base.add(y * stride + x * 4);
                    *pixel = b;
                    *pixel.add(1) = g;
                    *pixel.add(2) = r;
                    *pixel.add(3) = a;
                }
            }
            surface.unlock(IOSurfaceLockOptions::empty(), std::ptr::null_mut());
        }
        surface
    }
}
