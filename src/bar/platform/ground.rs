//! Picturing the desktop behind a bar for its ground: the wallpaper alone, so neither the desktop's
//! icons nor its widgets bleed into the blur. The arithmetic is `domain/ground.rs`.

use std::hash::{DefaultHasher, Hash, Hasher};

use objc2_core_foundation::{CFRetained, CGPoint, CGRect, CGSize};
use objc2_core_graphics::{
    CGBitmapContextCreateImage, CGBitmapContextGetBytesPerRow, CGBitmapContextGetData, CGColorSpace,
    CGContext, CGImage, kCGColorSpaceSRGB,
};

use crate::animation::platform::backdrop::desktop_backdrop_windows;
use crate::animation::platform::edge_dressing::rgba_bitmap_context;
use crate::bar::domain::ground::{self, BLUR};
use crate::bar::domain::palette::N1;
use crate::bar::platform::menu_extras::capture;

/// One display's ground, `scale` pixels per point.
pub struct Ground {
    pub display: String,
    pub picture: CFRetained<CGImage>,
    pub scale: f64,
}

/// The ground for the display at `bounds`, with its scale and a hash of what was captured, so an
/// unchanged wallpaper is not blurred and sent again. `None` when no wallpaper window is listed or
/// the capture fails.
pub fn picture_ground(bounds: CGRect) -> Option<(CFRetained<CGImage>, f64, u64)> {
    let wallpaper: Vec<u32> =
        desktop_backdrop_windows(bounds).wallpaper.iter().map(|id| id.as_u32()).collect();
    if wallpaper.is_empty() {
        return None;
    }
    let rect = ground::capture_rect(bounds);
    let captured = capture(&wallpaper, rect)?;
    let (width, height) = (CGImage::width(Some(&captured)), CGImage::height(Some(&captured)));
    if width == 0 || height == 0 {
        return None;
    }
    let scale = width as f64 / rect.size.width;
    let context = rgba_bitmap_context(width, height)?;
    let whole = CGRect::new(CGPoint::ZERO, CGSize::new(width as f64, height as f64));
    CGContext::draw_image(Some(&context), whole, Some(&captured));
    let data = CGBitmapContextGetData(Some(&context)) as *mut u8;
    if data.is_null() {
        return None;
    }
    let stride = CGBitmapContextGetBytesPerRow(Some(&context));
    // SAFETY: the context owns `stride * height` bytes and outlives this slice.
    let pixels = unsafe { std::slice::from_raw_parts_mut(data, stride * height) };
    let mut hasher = DefaultHasher::new();
    pixels.hash(&mut hasher);
    let hash = hasher.finish();

    ground::blur(pixels, width, height, stride, (BLUR * scale).round() as usize);
    ground::tint(pixels, N1);
    let drawn = CGBitmapContextCreateImage(Some(&context))?;
    let rows = ground::bar_rows(scale).min(height);
    let strip = CGRect::new(CGPoint::ZERO, CGSize::new(width as f64, rows as f64));
    let cropped = CGImage::with_image_in_rect(Some(&drawn), strip)?;
    // Tagged sRGB, as the bar's words are, so the ground colour is matched to the display the way the
    // palette is. SAFETY: a CoreGraphics constant, an immutable static.
    let srgb = CGColorSpace::with_name(Some(unsafe { kCGColorSpaceSRGB }))?;
    let picture = CGImage::new_copy_with_color_space(Some(&cropped), Some(&srgb))?;
    Some((picture, scale, hash))
}
