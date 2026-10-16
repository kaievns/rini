//! Pictures read on the service's thread: a picture's band print and its body samples, which say
//! whether it shows anything new (`crate::pictures::domain::print`), its thumbprint, which says
//! whether its window's rendering has settled (`crate::pictures::domain::settle`), and what a
//! window's first picture says of it (`crate::pictures::domain::knowledge`), each drawing only as
//! much of the picture as it needs; and the overlay and the screen drawn whole for a handover check
//! (`crate::pictures::domain::handover`). What a picture's ground is
//! (`crate::pictures::domain::ground`) and the space it is filled in are read on the thread that
//! asks (`ground_of`, `ground_space`), the overlay's main thread as it fills a crop-drawn tile.

use objc2_core_foundation::{CFRetained, CGPoint, CGRect, CGSize};
use objc2_core_graphics::{
    CGBitmapContextGetBytesPerRow, CGBitmapContextGetData, CGBitmapInfo, CGColorSpace,
    CGColorSpaceModel, CGContext, CGImage, CGImageAlphaInfo, CGImageByteOrderInfo,
    CGInterpolationQuality,
};
use objc2_io_surface::IOSurfaceRef;
use rini_geometry::space::ScreenPoint;

use crate::pictures::domain::ground::{Ground, ground};
use crate::pictures::domain::handover::Rgba;
use crate::pictures::domain::knowledge::{
    MATERIAL_SIDE, TRANSLUCENCY_SIDE, has_material, is_translucent,
};
use crate::pictures::domain::print::{
    BAND_PRINT, BAND_PT, BODY_ACROSS, BODY_DOWN, BODY_GRID, body_points,
};
use crate::pictures::platform::bitmap::{
    THUMBPRINT_SIDE, bitmap_context, rgba_bitmap_context, thumbprint,
};
use crate::pictures::platform::picture::{Picture, Pixels, image_size};
use crate::pictures::platform::surface::read_locked;

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

/// A picture's body sampled at [`body_points`], premultiplied RGBA, for `bodies_differ` and its
/// `ground`, in the colour space [`ground_space`] names.
pub fn body_samples(image: &Pixels) -> Option<Vec<[u8; 4]>> {
    match image {
        Pixels::Surface(surface) => surface_samples(surface),
        Pixels::Bitmap(_) | Pixels::Region { .. } => {
            let image = image.bitmap()?;
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
            let quality = CGInterpolationQuality::None;
            let ((bytes, stride), format) = match own_format(&image) {
                Some((format, space)) => {
                    let ctx = bitmap_context(BODY_GRID, BODY_GRID, &space, format.info)?;
                    (drawn_into(&ctx, &body, BODY_GRID, BODY_GRID, quality)?, format)
                }
                None => (drawn(&body, BODY_GRID, BODY_GRID, quality)?, RGBA),
            };
            let [r, g, b, a] = format.channels;
            Some(
                (0..BODY_GRID * BODY_GRID)
                    .map(|i| {
                        let at = (i / BODY_GRID) * stride + (i % BODY_GRID) * 4;
                        [bytes[at + r], bytes[at + g], bytes[at + b], bytes[at + a]]
                    })
                    .collect(),
            )
        }
    }
}

/// A picture's thumbprint: a bitmap's drawn small (`thumbprint`), a surface's read off its pixels
/// (`surface_thumbprint`). The two are never judged against each other: a window's repaint settles on
/// captures of one route.
pub fn picture_thumbprint(picture: &Picture) -> Option<Vec<u8>> {
    match &picture.image {
        Pixels::Surface(surface) => surface_thumbprint(surface),
        image => thumbprint(&*image.bitmap()?),
    }
}

/// A surface's thumbprint, BGRA read as RGBA: each cell of a `THUMBPRINT_SIDE` square the mean of
/// `THUMBPRINT_POINTS` squared points across it.
fn surface_thumbprint(surface: &IOSurfaceRef) -> Option<Vec<u8>> {
    const THUMBPRINT_POINTS: usize = 4;
    let (width, height, stride) = (surface.width(), surface.height(), surface.bytes_per_row());
    if width == 0 || height == 0 || stride < width * 4 {
        return None;
    }
    let (side, points) = (THUMBPRINT_SIDE, THUMBPRINT_POINTS);
    // The middle of the `at`th of `side * points` equal steps across `length` pixels.
    let step = |at: usize, length: usize| (at * 2 + 1) * length / (side * points * 2);
    read_locked(surface, |pixels| {
        let mut print = Vec::with_capacity(side * side * 4);
        for row in 0..side {
            for column in 0..side {
                let mut sum = [0usize; 4];
                for down in 0..points {
                    let y = step(row * points + down, height);
                    for across in 0..points {
                        let pixel = y * stride + step(column * points + across, width) * 4;
                        for (channel, at) in [2, 1, 0, 3].into_iter().enumerate() {
                            sum[channel] += pixels[pixel + at] as usize;
                        }
                    }
                }
                print.extend(sum.map(|total| (total / (points * points)) as u8));
            }
        }
        print
    })
}

/// The ground of a picture's pixels, read off its body samples, in the colour space
/// [`ground_space`] names.
pub fn ground_of(image: &Pixels) -> Option<Ground> { ground(&body_samples(image)?) }

/// The colour space a picture's body is sampled in, and so its ground filled in: a bitmap's own,
/// read in its own pixel format, and device RGB (`None`) for a surface and for a bitmap whose format
/// [`sample_format`] cannot read.
pub fn ground_space(image: &Pixels) -> Option<CFRetained<CGColorSpace>> {
    own_format(&*image.bitmap()?).map(|(_, space)| space)
}

/// How a 32-bit premultiplied pixel holds its channels: the byte each of red, green, blue and alpha
/// is at, and the bitmap info a context drawing such pixels is made with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SampleFormat {
    pub channels: [usize; 4],
    pub info: CGBitmapInfo,
}

/// The format of the device RGB context a picture is drawn into when its own cannot be read.
const RGBA: SampleFormat = SampleFormat {
    channels: [0, 1, 2, 3],
    info: CGBitmapInfo(CGImageAlphaInfo::PremultipliedLast.0),
};

/// How to read the pixels of an image in an RGB space of `bits_per_component` and `bits_per_pixel`
/// with `info`, drawn into a context of the same: 8 bits a channel, four to a pixel, alpha
/// premultiplied first or last, in either 32-bit byte order. `None` for any other, 16 bits, float,
/// packed or without alpha, which a context of its own would not take or which is not read as bytes.
pub fn sample_format(
    model: CGColorSpaceModel,
    info: CGBitmapInfo,
    bits_per_component: usize,
    bits_per_pixel: usize,
) -> Option<SampleFormat> {
    let extra = info.0 & !(CGBitmapInfo::AlphaInfoMask.0 | CGBitmapInfo::ByteOrderInfoMask.0);
    if model != CGColorSpaceModel::RGB
        || bits_per_component != 8
        || bits_per_pixel != 32
        || extra != 0
    {
        return None;
    }
    let alpha = CGImageAlphaInfo(info.0 & CGBitmapInfo::AlphaInfoMask.0);
    let order = CGImageByteOrderInfo(info.0 & CGBitmapInfo::ByteOrderInfoMask.0);
    let little = match order {
        CGImageByteOrderInfo::Order32Little => true,
        CGImageByteOrderInfo::OrderDefault | CGImageByteOrderInfo::Order32Big => false,
        _ => return None,
    };
    let channels = match (alpha, little) {
        (CGImageAlphaInfo::PremultipliedFirst, true) => [2, 1, 0, 3],
        (CGImageAlphaInfo::PremultipliedFirst, false) => [1, 2, 3, 0],
        (CGImageAlphaInfo::PremultipliedLast, true) => [3, 2, 1, 0],
        (CGImageAlphaInfo::PremultipliedLast, false) => [0, 1, 2, 3],
        _ => return None,
    };
    Some(SampleFormat { channels, info })
}

/// `image`'s own colour space and the format its pixels are read in, when [`sample_format`] reads
/// them: drawn into a context of both, nothing of the image is converted.
fn own_format(image: &CGImage) -> Option<(SampleFormat, CFRetained<CGColorSpace>)> {
    let space = CGImage::color_space(Some(image))?;
    let format = sample_format(
        CGColorSpace::model(Some(&space)),
        CGImage::bitmap_info(Some(image)),
        CGImage::bits_per_component(Some(image)),
        CGImage::bits_per_pixel(Some(image)),
    )?;
    Some((format, space))
}

/// A surface's pixels at the body's points, BGRA read as RGBA.
fn surface_samples(surface: &IOSurfaceRef) -> Option<Vec<[u8; 4]>> {
    let (width, height, stride) = (surface.width(), surface.height(), surface.bytes_per_row());
    if width == 0 || height == 0 || stride < width * 4 {
        return None;
    }
    read_locked(surface, |pixels| {
        body_points()
            .map(|(u, v)| {
                let x = ((u * width as f64) as usize).min(width - 1);
                let y = ((v * height as f64) as usize).min(height - 1);
                let pixel = y * stride + x * 4;
                [
                    pixels[pixel + 2],
                    pixels[pixel + 1],
                    pixels[pixel],
                    pixels[pixel + 3],
                ]
            })
            .collect()
    })
}

/// What a window's first picture, `crop`, says of it: whether it is translucent, from the picture
/// drawn small.
pub fn first_sight(crop: &CGImage) -> bool {
    drawn(
        crop,
        TRANSLUCENCY_SIDE,
        TRANSLUCENCY_SIDE,
        CGInterpolationQuality::Default,
    )
    .is_some_and(|(bytes, stride)| is_translucent(&bytes, stride))
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
        Rgba {
            width: self.width,
            height: self.height,
            stride: self.stride,
            bytes: &self.bytes,
            origin,
            scale,
        }
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
    drawn_into(&ctx, image, width, height, quality)
}

/// `image` drawn into `ctx`, a fresh `width` x `height` bitmap context, and its bytes top row first,
/// with the stride.
fn drawn_into(
    ctx: &CGContext,
    image: &CGImage,
    width: usize,
    height: usize,
    quality: CGInterpolationQuality,
) -> Option<(Vec<u8>, usize)> {
    let ctx = Some(ctx);
    CGContext::set_interpolation_quality(ctx, quality);
    CGContext::draw_image(
        ctx,
        CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(width as f64, height as f64)),
        Some(image),
    );
    let data = CGBitmapContextGetData(ctx) as *const u8;
    if data.is_null() {
        return None;
    }
    let stride = CGBitmapContextGetBytesPerRow(ctx);
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
    use crate::pictures::platform::surface::{Layout, new_surface};

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
        let a = body_samples(&grey.image).unwrap();
        assert_eq!(a.len(), BODY_GRID * BODY_GRID);
        assert!(
            a.iter()
                .all(|rgba| rgba[..3].iter().all(|c| c.abs_diff(128) <= 1) && rgba[3] == 255),
            "grey body"
        );

        let mut surface = test_picture(ScreenSize::new(400.0, 300.0));
        surface.image = Pixels::Surface(test_surface(800, 600, [128, 128, 128, 255]));
        assert!(!bodies_differ(&a, &body_samples(&surface.image).unwrap()));

        surface.image = Pixels::Surface(test_surface(800, 600, [20, 20, 20, 255]));
        assert!(bodies_differ(&a, &body_samples(&surface.image).unwrap()));
        assert!(
            !bodies_differ(
                &a,
                &body_samples(&window((400.0, 300.0), 2.0, (0.1, 0.1, 0.1)).image).unwrap()
            ),
            "the band is not the body"
        );
    }

    /// A picture's ground is its body's colour, whatever its title band, from a bitmap and from a
    /// surface alike; a translucent surface keeps its alpha.
    #[test]
    fn a_pictures_ground_is_read_off_its_body() {
        let near = |got: Ground, want: [f64; 4]| {
            let got = [got.r, got.g, got.b, got.a];
            got.iter().zip(want).all(|(got, want)| (got - want).abs() < 0.01)
        };
        let grey = ground_of(&window((400.0, 300.0), 2.0, (0.9, 0.9, 0.9)).image).unwrap();
        assert!(near(grey, [0.5, 0.5, 0.5, 1.0]), "{grey:?}");
        let surface = Pixels::Surface(test_surface(800, 600, [40, 20, 10, 255]));
        let brown = ground_of(&surface).unwrap();
        assert!(
            near(brown, [40.0 / 255.0, 20.0 / 255.0, 10.0 / 255.0, 1.0]),
            "{brown:?}"
        );
        let surface = Pixels::Surface(test_surface(800, 600, [95, 95, 95, 242]));
        let clear = ground_of(&surface).unwrap();
        let (level, alpha) = (95.0 / 242.0, 242.0 / 255.0);
        assert!(
            near(clear, [level, level, level, alpha]),
            "unpremultiplied: {clear:?}"
        );
    }

    /// A window picture in Display P3, BGRA premultiplied first in little endian, flat in `p3`,
    /// components in its own space.
    fn p3_image(width: usize, height: usize, p3: [f64; 4]) -> CFRetained<CGImage> {
        let space = display_p3();
        let bgra = CGImageAlphaInfo::PremultipliedFirst.0 | CGImageByteOrderInfo::Order32Little.0;
        let ctx = bitmap_context(width, height, &space, CGBitmapInfo(bgra)).unwrap();
        // SAFETY: Display P3 is RGB, so the call reads three components and alpha, all of `p3`.
        let colour =
            unsafe { objc2_core_graphics::CGColor::new(Some(&space), p3.as_ptr()) }.unwrap();
        CGContext::set_fill_color_with_color(Some(&ctx), Some(&colour));
        CGContext::fill_rect(Some(&ctx), rect(0.0, 0.0, width as f64, height as f64));
        CGBitmapContextCreateImage(Some(&ctx)).unwrap()
    }

    fn display_p3() -> CFRetained<CGColorSpace> {
        // SAFETY: Core Graphics' own colour space name.
        CGColorSpace::with_name(Some(unsafe { objc2_core_graphics::kCGColorSpaceDisplayP3 }))
            .unwrap()
    }

    fn is_p3(space: Option<&CGColorSpace>) -> bool {
        // SAFETY: Core Graphics' own colour space name.
        let p3 = unsafe { objc2_core_graphics::kCGColorSpaceDisplayP3 };
        CGColorSpace::name(space).is_some_and(|name| *name == *p3)
    }

    /// BGRA, premultiplied first in little endian, is read with red at byte 2, and every other 8-bit
    /// premultiplied layout at its own bytes.
    #[test]
    fn sample_format_reads_bgra_premultiplied_first_as_rgba() {
        let rgb = CGColorSpaceModel::RGB;
        let info =
            |alpha: CGImageAlphaInfo, order: CGImageByteOrderInfo| CGBitmapInfo(alpha.0 | order.0);
        let (first, last) = (
            CGImageAlphaInfo::PremultipliedFirst,
            CGImageAlphaInfo::PremultipliedLast,
        );
        let (little, big) = (
            CGImageByteOrderInfo::Order32Little,
            CGImageByteOrderInfo::Order32Big,
        );
        let bgra = info(first, little);
        assert_eq!(
            sample_format(rgb, bgra, 8, 32),
            Some(SampleFormat {
                channels: [2, 1, 0, 3],
                info: bgra
            })
        );
        assert_eq!(
            sample_format(rgb, info(first, big), 8, 32).unwrap().channels,
            [1, 2, 3, 0],
            "ARGB"
        );
        assert_eq!(
            sample_format(rgb, info(last, little), 8, 32).unwrap().channels,
            [3, 2, 1, 0],
            "ABGR"
        );
        let rgba = info(last, CGImageByteOrderInfo::OrderDefault);
        assert_eq!(
            sample_format(rgb, rgba, 8, 32).unwrap().channels,
            [0, 1, 2, 3],
            "RGBA"
        );
    }

    /// What a context of the image's own format would not take, or that is not read a byte a channel,
    /// goes through the device RGB draw: 16 bits a channel, float, no alpha, alpha not premultiplied,
    /// 16-bit byte order, a packed format, and a space that is not RGB.
    #[test]
    fn sample_format_refuses_16_bit_and_float() {
        let rgb = CGColorSpaceModel::RGB;
        let bgra = CGImageAlphaInfo::PremultipliedFirst.0 | CGImageByteOrderInfo::Order32Little.0;
        assert!(
            sample_format(rgb, CGBitmapInfo(bgra), 16, 64).is_none(),
            "16-bit"
        );
        let float = CGImageAlphaInfo::PremultipliedLast.0
            | objc2_core_graphics::CGImageComponentInfo::Float.0;
        assert!(
            sample_format(rgb, CGBitmapInfo(float), 32, 128).is_none(),
            "float"
        );
        assert!(
            sample_format(rgb, CGBitmapInfo(float), 8, 32).is_none(),
            "the float flag alone"
        );
        assert!(
            sample_format(
                rgb,
                CGBitmapInfo(float | CGImageByteOrderInfo::Order16Little.0),
                16,
                64
            )
            .is_none(),
            "half float"
        );
        for alpha in [
            CGImageAlphaInfo::NoneSkipFirst,
            CGImageAlphaInfo::NoneSkipLast,
            CGImageAlphaInfo::None,
            CGImageAlphaInfo::First,
            CGImageAlphaInfo::Last,
        ] {
            assert!(
                sample_format(rgb, CGBitmapInfo(alpha.0), 8, 32).is_none(),
                "{alpha:?}"
            );
        }
        let sixteen =
            CGImageAlphaInfo::PremultipliedFirst.0 | CGImageByteOrderInfo::Order16Little.0;
        assert!(
            sample_format(rgb, CGBitmapInfo(sixteen), 8, 32).is_none(),
            "16-bit order"
        );
        assert!(
            sample_format(
                rgb,
                CGBitmapInfo(bgra | objc2_core_graphics::CGImagePixelFormatInfo::RGB101010.0),
                8,
                32
            )
            .is_none(),
            "packed"
        );
        assert!(
            sample_format(CGColorSpaceModel::Monochrome, CGBitmapInfo(bgra), 8, 32).is_none(),
            "grey"
        );
    }

    /// A Display P3 picture's ground is its own P3 components, read without converting the picture,
    /// and is filled in P3: read in device RGB, P3's saturated colours would come out shifted, and
    /// the fill beside the picture tinted.
    #[test]
    fn a_p3_pictures_ground_is_read_in_p3() {
        let p3 = [0.2, 0.5, 0.8, 1.0];
        let picture = Pixels::Bitmap(p3_image(800, 600, p3));
        let got = ground_of(&picture).expect("a ground");
        let got = [got.r, got.g, got.b, got.a];
        assert!(
            got.iter().zip(p3).all(|(got, want)| (got - want).abs() <= 1.0 / 255.0),
            "{got:?}"
        );
        assert!(is_p3(ground_space(&picture).as_deref()), "kept in P3");

        let surface = Pixels::Surface(test_surface(8, 8, [51, 128, 204, 255]));
        assert!(ground_space(&surface).is_none(), "a surface is device RGB");
        assert!(
            ground_space(&window((40.0, 30.0), 1.0, (0.5, 0.5, 0.5)).image)
                .is_some_and(|space| !is_p3(Some(&space))),
            "a device RGB picture stays so"
        );
    }

    /// A P3 bitmap and a surface holding its bytes are sampled alike, so a focused face kept from one
    /// is not dropped for a newer picture of the other route showing the same window.
    #[test]
    fn a_p3_bitmap_and_a_surface_of_its_colour_do_not_differ() {
        let bitmap =
            body_samples(&Pixels::Bitmap(p3_image(800, 600, [0.2, 0.5, 0.8, 1.0]))).unwrap();
        let surface =
            body_samples(&Pixels::Surface(test_surface(800, 600, [51, 128, 204, 255]))).unwrap();
        assert!(!bodies_differ(&bitmap, &surface));
        let other =
            body_samples(&Pixels::Surface(test_surface(800, 600, [80, 128, 204, 255]))).unwrap();
        assert!(bodies_differ(&bitmap, &other));
    }

    /// First sight of an opaque window with rounded corners, and of a translucent one.
    #[test]
    fn first_sight_finds_translucency() {
        let (w, h) = (400usize, 300usize);
        let radius = 20.0;
        let rounded = image(w, h, |ctx| {
            // SAFETY: a null transform is allowed, and means none.
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
        assert!(!first_sight(&rounded), "opaque");

        let glass = image(w, h, |ctx| {
            fill(ctx, rect(0.0, 0.0, w as f64, h as f64), (0.1, 0.1, 0.1, 0.6))
        });
        assert!(first_sight(&glass), "translucent");
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
        assert_eq!(
            (read.origin, read.scale, read.bytes.len()),
            (ScreenPoint::new(10.0, 20.0), 2.0, drawn.stride * 2)
        );
    }

    /// A surface is thumbprinted off its pixels, read as RGBA, into one small square whatever its
    /// size: the same content prints the same, other content otherwise.
    #[test]
    fn a_surface_is_thumbprinted_into_one_small_square_whatever_its_size() {
        let print = |width, height, rgba| {
            surface_thumbprint(&test_surface(width, height, rgba)).expect("printed")
        };
        let small = print(60, 40, [10, 20, 30, 255]);
        assert_eq!(small.len(), THUMBPRINT_SIDE * THUMBPRINT_SIDE * 4);
        assert_eq!(small[..4], [10, 20, 30, 255], "RGBA");
        assert_eq!(small, print(1148, 1079, [10, 20, 30, 255]));
        assert_ne!(small, print(60, 40, [200, 20, 30, 255]));
    }

    fn test_surface(
        width: usize,
        height: usize,
        bgra_of_rgba: [u8; 4],
    ) -> CFRetained<IOSurfaceRef> {
        let [r, g, b, a] = bgra_of_rgba;
        new_surface(width, height, Layout::default(), |bytes, stride| {
            for y in 0..height {
                for x in 0..width {
                    let pixel = y * stride + x * 4;
                    bytes[pixel..pixel + 4].copy_from_slice(&[b, g, r, a]);
                }
            }
            Some(())
        })
        .expect("a surface")
    }
}
