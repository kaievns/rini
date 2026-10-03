//! Putting a window's blur back into its picture.
//!
//! A capture of a window on its own never contains a blurred material — a sidebar's vibrancy, a
//! terminal's glass, a Liquid Glass toolbar — because the window server blurs whatever is behind the
//! window at composite time, outside the window's surface. Every per-window route returns the same flat
//! grey there, fully opaque. A capture of the window WITH everything below it does contain the blur,
//! and leaves out anything above it. See "A composite of the window and what is below it carries the
//! blur" in `src/animation/docs/capture-overlay-research.md`.
//!
//! Neither capture is the picture on its own. The composite has no transparency: a rounded corner comes
//! back filled with whatever was behind it. The window's own capture has the right transparency and the
//! wrong colour wherever the blur is. So the two are merged, per pixel:
//!
//! - where the window's own pixel is **not** fully opaque — a rounded corner, or an application drawing
//!   real transparency, which captures correctly on its own — the window's own pixel stands;
//! - where it **is** fully opaque, the composite's colour is taken. For genuinely opaque content the two
//!   are identical, so nothing changes; for a blurred material this replaces the grey with the blur.

/// How far apart two opaque pixels have to be, summed over the three channels, to count as the
/// window's own capture having been wrong there rather than as rounding.
const DIFFERENT: u32 = 6;

/// The share of a picture that has to have changed for it to count as carrying a blur the window's own
/// capture lacked, in parts per thousand. Opaque content is pixel-identical between the two captures,
/// so anything above rounding noise is a material.
const CARRIES_BLUR_PER_MILLE: usize = 2;

/// Two same-sized RGBA pictures, 4 bytes a pixel with alpha last, each with its own row stride.
pub struct Pictures<'a> {
    /// The window on its own. Overwritten with the merged picture.
    pub own: &'a mut [u8],
    pub own_stride: usize,
    /// The window with everything below it.
    pub composite: &'a [u8],
    pub composite_stride: usize,
    pub width: usize,
    pub height: usize,
}

/// What a merge did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Merged {
    /// Opaque pixels whose colour the composite changed materially.
    pub changed: usize,
    /// Whether that is enough to say the window has a blurred material, so the merged picture is worth
    /// more than the window's own capture of the same size.
    pub carries_blur: bool,
}

/// Merge `pictures.composite` into `pictures.own` by the rule in the module docs.
///
/// Returns `None` when the buffers are too small for the stated size, so a caller cannot read past
/// either one.
pub fn merge(pictures: Pictures<'_>) -> Option<Merged> {
    let Pictures {
        own,
        own_stride,
        composite,
        composite_stride,
        width,
        height,
    } = pictures;
    let row = width.checked_mul(4)?;
    if width == 0 || height == 0 || own_stride < row || composite_stride < row {
        return None;
    }
    let needs = |stride: usize| stride.checked_mul(height - 1)?.checked_add(row);
    if own.len() < needs(own_stride)? || composite.len() < needs(composite_stride)? {
        return None;
    }

    let mut changed = 0usize;
    for y in 0..height {
        let own_row = &mut own[y * own_stride..y * own_stride + row];
        let composite_row = &composite[y * composite_stride..y * composite_stride + row];
        for (mine, theirs) in own_row.chunks_exact_mut(4).zip(composite_row.chunks_exact(4)) {
            if mine[3] != 255 {
                continue;
            }
            let distance: u32 = (0..3).map(|c| mine[c].abs_diff(theirs[c]) as u32).sum();
            if distance > DIFFERENT {
                changed += 1;
            }
            mine[..3].copy_from_slice(&theirs[..3]);
        }
    }
    Some(Merged {
        changed,
        carries_blur: changed * 1000 > width * height * CARRIES_BLUR_PER_MILLE,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn picture(width: usize, height: usize, rgba: [u8; 4]) -> Vec<u8> {
        rgba.iter().copied().cycle().take(width * height * 4).collect()
    }

    fn merge_all(own: &mut [u8], composite: &[u8], width: usize, height: usize) -> Merged {
        merge(Pictures {
            own,
            own_stride: width * 4,
            composite,
            composite_stride: width * 4,
            width,
            height,
        })
        .expect("well-formed")
    }

    /// The reported case: the window's own capture of a blurred sidebar is flat grey, and the composite
    /// has the blur. The grey is replaced, and the picture is marked as carrying a blur.
    #[test]
    fn the_grey_where_a_blur_belongs_is_replaced_by_the_blur() {
        let (w, h) = (10, 10);
        let mut own = picture(w, h, [84, 84, 83, 255]);
        let composite = picture(w, h, [55, 59, 53, 255]);

        let merged = merge_all(&mut own, &composite, w, h);

        assert_eq!(&own[..4], &[55, 59, 53, 255]);
        assert_eq!(merged.changed, w * h);
        assert!(merged.carries_blur);
    }

    /// Opaque content is pixel-identical in the two captures, so a window with no material comes out
    /// unchanged and is not marked.
    #[test]
    fn opaque_content_is_untouched_and_unmarked() {
        let (w, h) = (10, 10);
        let mut own = picture(w, h, [52, 40, 33, 255]);
        let composite = own.clone();

        let merged = merge_all(&mut own, &composite, w, h);

        assert_eq!(own, composite);
        assert_eq!(merged.changed, 0);
        assert!(!merged.carries_blur);
    }

    /// A rounded corner is transparent in the window's own capture and filled with the backdrop in the
    /// composite. The corner must stay transparent, or the tile carries a baked square of desktop.
    #[test]
    fn a_transparent_corner_stays_transparent() {
        let (w, h) = (4, 4);
        let mut own = picture(w, h, [0, 0, 0, 0]);
        let composite = picture(w, h, [250, 200, 0, 255]);

        merge_all(&mut own, &composite, w, h);

        assert_eq!(&own[..4], &[0, 0, 0, 0]);
    }

    /// Real per-pixel transparency is captured correctly on its own — a terminal at 90% opacity with
    /// no blur reads alpha 230 — and the composite has the backdrop mixed into those pixels. Taking the
    /// composite there would count the backdrop twice once the tile is drawn over the desktop.
    #[test]
    fn real_transparency_keeps_the_windows_own_pixel() {
        let (w, h) = (4, 4);
        let mut own = picture(w, h, [14, 15, 16, 230]);
        let composite = picture(w, h, [40, 60, 90, 255]);

        let merged = merge_all(&mut own, &composite, w, h);

        assert_eq!(&own[..4], &[14, 15, 16, 230]);
        assert_eq!(merged.changed, 0);
    }

    /// Rounding differences between two captures of the same pixels are not a material.
    #[test]
    fn rounding_noise_is_not_a_blur() {
        let (w, h) = (10, 10);
        let mut own = picture(w, h, [52, 40, 33, 255]);
        let composite = picture(w, h, [53, 41, 34, 255]);

        let merged = merge_all(&mut own, &composite, w, h);

        assert_eq!(merged.changed, 0);
        assert!(!merged.carries_blur);
    }

    /// A small blurred area — a toolbar strip across a big window — still counts, as long as it is above
    /// the noise floor.
    #[test]
    fn a_blurred_toolbar_across_a_large_window_counts() {
        let (w, h) = (100, 100);
        let mut own = picture(w, h, [30, 30, 30, 255]);
        let mut composite = own.clone();
        for pixel in composite.chunks_exact_mut(4).take(w * 5) {
            pixel.copy_from_slice(&[70, 60, 90, 255]);
        }

        let merged = merge_all(&mut own, &composite, w, h);

        assert_eq!(merged.changed, w * 5);
        assert!(merged.carries_blur, "a 5% strip is a material");
    }

    /// Strides wider than a row are how bitmap contexts pad; the padding is never read or written.
    #[test]
    fn padded_rows_are_respected() {
        let (w, h) = (2, 2);
        let stride = w * 4 + 8;
        let mut own = vec![0u8; stride * h];
        let mut composite = vec![0u8; stride * h];
        for y in 0..h {
            own[y * stride..y * stride + w * 4]
                .copy_from_slice(&[84, 84, 83, 255, 84, 84, 83, 255]);
            composite[y * stride..y * stride + w * 4]
                .copy_from_slice(&[55, 59, 53, 255, 55, 59, 53, 255]);
            own[y * stride + w * 4] = 7;
        }

        merge(Pictures {
            own: &mut own,
            own_stride: stride,
            composite: &composite,
            composite_stride: stride,
            width: w,
            height: h,
        })
        .expect("well-formed");

        assert_eq!(&own[..4], &[55, 59, 53, 255]);
        assert_eq!(own[w * 4], 7, "padding untouched");
    }

    #[test]
    fn buffers_too_small_for_the_stated_size_are_refused() {
        let mut own = vec![0u8; 12];
        let composite = vec![0u8; 16];
        assert_eq!(
            merge(Pictures {
                own: &mut own,
                own_stride: 8,
                composite: &composite,
                composite_stride: 8,
                width: 2,
                height: 2,
            }),
            None
        );
    }
}
