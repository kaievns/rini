//! The bar's ground: a still picture of the desktop behind the bar, blurred, under the ground colour.
//!
//! A still picture rather than a live blur, because a live blur of what is behind the bar is redone
//! by the window server on every frame something moves behind it, and the flight overlay moves behind
//! it on every frame of a flight. See "The ground is a still picture" in `src/bar/docs/README.md`.

use std::time::{Duration, Instant};

use objc2_core_foundation::{CGPoint, CGRect, CGSize};

use super::layout::HEIGHT;
use super::palette::{BAR_GROUND_ALPHA, Colour};

/// How far the blur spreads, in points: about the standard deviation of a Gaussian.
pub const BLUR: f64 = 12.0;

/// How much of the desktop below the bar is pictured with it, so the blur's lower edge draws on what
/// is really there. Three spreads is where a Gaussian has all but died out.
const BELOW: f64 = 3.0 * BLUR;

/// How often the wallpaper is pictured again to see whether it changed. A wallpaper that follows the
/// time of day changes on its own, and nothing says when.
pub const EVERY: Duration = Duration::from_secs(60);

/// What to picture for a display whose bounds are `display`: the bar's strip and the margin below it.
pub fn capture_rect(display: CGRect) -> CGRect {
    CGRect::new(
        CGPoint::new(display.origin.x, display.origin.y),
        CGSize::new(display.size.width, HEIGHT + BELOW),
    )
}

/// How many pixel rows of a capture at `scale` are the bar itself, the rest being margin.
pub fn bar_rows(scale: f64) -> usize {
    (HEIGHT * scale).round() as usize
}

/// Blurs an RGBA picture in place: three box blurs each way, which together come within a few per
/// cent of a Gaussian of `spread` pixels. Beyond the picture's edges its edge pixels carry on.
pub fn blur(pixels: &mut [u8], width: usize, height: usize, stride: usize, spread: usize) {
    if width == 0 || height == 0 || spread == 0 {
        return;
    }
    let mut line = Vec::new();
    for _ in 0..3 {
        for y in 0..height {
            let row = y * stride;
            box_line(pixels, row, 4, width, spread, &mut line);
        }
        for x in 0..width {
            box_line(pixels, x * 4, stride, height, spread, &mut line);
        }
    }
}

/// One box blur of radius `radius` along `count` pixels starting at byte `start`, `step` bytes apart.
fn box_line(pixels: &mut [u8], start: usize, step: usize, count: usize, radius: usize, line: &mut Vec<[u8; 4]>) {
    line.clear();
    line.extend((0..count).map(|i| {
        let at = start + i * step;
        [pixels[at], pixels[at + 1], pixels[at + 2], pixels[at + 3]]
    }));
    let clamped = |i: isize| line[i.clamp(0, count as isize - 1) as usize];
    let window = (2 * radius + 1) as u32;
    let mut sums = [0u32; 4];
    for i in -(radius as isize)..=(radius as isize) {
        for (sum, value) in sums.iter_mut().zip(clamped(i)) {
            *sum += value as u32;
        }
    }
    for i in 0..count {
        let at = start + i * step;
        for channel in 0..4 {
            pixels[at + channel] = ((sums[channel] + window / 2) / window) as u8;
        }
        let (leaving, entering) = (clamped(i as isize - radius as isize), clamped(i as isize + radius as isize + 1));
        for channel in 0..4 {
            sums[channel] = sums[channel] + entering[channel] as u32 - leaving[channel] as u32;
        }
    }
}

/// Lays the ground colour over an opaque RGBA picture, at the ground's opacity.
pub fn tint(pixels: &mut [u8], colour: Colour) {
    let (red, green, blue) = colour.rgb();
    let over = [red, green, blue].map(|channel| channel * 255.0 * BAR_GROUND_ALPHA);
    for pixel in pixels.chunks_exact_mut(4) {
        for channel in 0..3 {
            let under = pixel[channel] as f64 * (1.0 - BAR_GROUND_ALPHA);
            pixel[channel] = (over[channel] + under).round().clamp(0.0, 255.0) as u8;
        }
        pixel[3] = 255;
    }
}

/// When the grounds are next pictured.
#[derive(Clone, Copy, Debug)]
pub struct Timer {
    due: Instant,
}

impl Timer {
    /// Due at once.
    pub fn new(now: Instant) -> Timer {
        Timer { due: now }
    }

    /// Picture at the next chance: the displays changed, or the machine woke.
    pub fn now(&mut self, now: Instant) {
        self.due = now;
    }

    /// Whether to picture now. Taking one puts the next `EVERY` away.
    pub fn take(&mut self, now: Instant) -> bool {
        if now < self.due {
            return false;
        }
        self.due = now + EVERY;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bar::domain::palette::N1;

    fn picture(width: usize, height: usize, fill: [u8; 4]) -> Vec<u8> {
        (0..width * height).flat_map(|_| fill).collect()
    }

    /// The strip itself and three spreads of the desktop below it, from the display's top edge.
    #[test]
    fn the_capture_covers_the_bar_and_the_margin_below() {
        let display = CGRect::new(CGPoint::new(-670.0, -1692.0), CGSize::new(3008.0, 1692.0));
        let rect = capture_rect(display);
        assert_eq!(rect.origin, display.origin);
        assert_eq!(rect.size, CGSize::new(3008.0, HEIGHT + 3.0 * BLUR));
        assert_eq!(bar_rows(2.0), 64);
    }

    /// A flat picture stays flat, edges included.
    #[test]
    fn a_blur_leaves_a_flat_picture_alone() {
        let mut pixels = picture(40, 12, [30, 60, 90, 255]);
        blur(&mut pixels, 40, 12, 40 * 4, 5);
        assert!(pixels.chunks_exact(4).all(|pixel| pixel == [30, 60, 90, 255]));
    }

    /// One bright pixel spreads evenly either side and keeps about its brightness in all.
    #[test]
    fn a_blur_spreads_a_point_evenly() {
        let (width, height) = (61, 61);
        let mut pixels = picture(width, height, [0, 0, 0, 255]);
        let centre = (30 * width + 30) * 4;
        pixels[centre] = 255;
        blur(&mut pixels, width, height, width * 4, 3);
        let red = |x: usize, y: usize| pixels[(y * width + x) * 4] as i32;
        assert_eq!(red(27, 30), red(33, 30));
        assert_eq!(red(30, 27), red(30, 33));
        assert!(red(30, 30) < 255 && red(30, 30) > 0);
        let total: i32 = (0..width * height).map(|i| pixels[i * 4] as i32).sum();
        assert!((total - 255).abs() < 60, "{total}");
    }

    /// Rows are `stride` bytes apart, which can be more than the pixels in them.
    #[test]
    fn a_blur_keeps_to_the_pixels_in_a_padded_row() {
        let (width, height, stride) = (4, 3, 32);
        let mut pixels = vec![7u8; stride * height];
        for y in 0..height {
            for byte in &mut pixels[y * stride..y * stride + width * 4] {
                *byte = 100;
            }
        }
        blur(&mut pixels, width, height, stride, 2);
        for y in 0..height {
            assert!(pixels[y * stride + width * 4..(y + 1) * stride].iter().all(|&byte| byte == 7));
        }
    }

    /// The ground colour at its opacity over whatever is there, and the result opaque.
    #[test]
    fn the_tint_is_the_ground_colour_over_the_picture() {
        let mut pixels = vec![255, 255, 255, 255, 0, 0, 0, 255];
        tint(&mut pixels, N1);
        let (red, ..) = N1.rgb();
        let expect_white = (red * 255.0 * BAR_GROUND_ALPHA + 255.0 * (1.0 - BAR_GROUND_ALPHA)).round() as u8;
        let expect_black = (red * 255.0 * BAR_GROUND_ALPHA).round() as u8;
        assert_eq!((pixels[0], pixels[3]), (expect_white, 255));
        assert_eq!((pixels[4], pixels[7]), (expect_black, 255));
    }

    /// Due at once, then once a minute, and at once again when asked.
    #[test]
    fn the_ground_is_pictured_at_once_then_each_minute() {
        let start = Instant::now();
        let mut timer = Timer::new(start);
        assert!(timer.take(start));
        assert!(!timer.take(start + Duration::from_secs(59)));
        assert!(timer.take(start + EVERY));
        timer.now(start + Duration::from_secs(61));
        assert!(timer.take(start + Duration::from_secs(61)));
    }
}
