//! What rini learns about each window the first time it sees it on screen: whether it is translucent,
//! and whether it has a material that blurs what is behind it. What is learned
//! decides which SkyLight call a window goes in. Pure: the pixels arrive as plain bytes, drawn small
//! by `crate::pictures::platform::measure`.
//!
//! Measured in "SLSHWCaptureWindowList captures only the visible portion" and "Translucency is three
//! things, and only one of them is lost" in `src/pictures/docs/capture-research.md`.

use std::collections::HashMap;

use rini_core::ids::WindowServerId;

/// What is known of one window. `None` until measured.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Knowledge {
    /// Per-pixel transparency worth keeping: what is behind it shows through.
    pub translucent: Option<bool>,
    /// A material that samples what is behind the window, which only a capture with the desktop after
    /// it carries.
    pub material: Option<bool>,
}

impl Knowledge {
    /// Nothing behind the window shows through it: measured opaque, and no material.
    pub fn is_opaque(&self) -> bool {
        self.translucent == Some(false) && self.material == Some(false)
    }

    /// Which call a window is captured in first.
    pub fn first_call(&self) -> Call {
        if self.translucent == Some(false) && self.material == Some(true) {
            Call::Baked
        } else {
            Call::Alpha
        }
    }

    /// After the alpha call, measured: whether the window keeps its picture from it, or joins the
    /// baked call, which a window opaque and not known to lack a material does.
    pub fn after_alpha(&self) -> Call {
        if self.translucent != Some(false) || self.material == Some(false) {
            Call::Alpha
        } else {
            Call::Baked
        }
    }
}

/// The two SkyLight calls a window can be pictured in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Call {
    /// The listed windows alone, each with its own alpha: a translucent window stays translucent, so
    /// the overlay draws what is really behind it.
    Alpha,
    /// The listed windows, then what is behind them, then the desktop: an opaque window's material
    /// carries the blur of what is below it.
    Baked,
}

/// Every window's knowledge, by window server id.
#[derive(Clone, Default)]
pub struct Learned {
    windows: HashMap<WindowServerId, Knowledge>,
}

impl Learned {
    pub fn get(&self, window: WindowServerId) -> Knowledge {
        self.windows.get(&window).copied().unwrap_or_default()
    }

    /// What the first alpha capture showed.
    pub fn saw_alpha(&mut self, window: WindowServerId, translucent: bool) {
        self.windows.entry(window).or_default().translucent = Some(translucent);
    }

    /// What comparing the window alone with it baked showed.
    pub fn saw_material(&mut self, window: WindowServerId, material: bool) {
        self.windows.entry(window).or_default().material = Some(material);
    }

    /// A window that is gone: its id may be handed to another.
    pub fn forget(&mut self, window: WindowServerId) {
        self.windows.remove(&window);
    }
}

/// The side of the square a picture is drawn into to judge its translucency, and the inset of the grid
/// judged inside it, which leaves the rounded corners out.
pub const TRANSLUCENCY_SIDE: usize = 48;
const TRANSLUCENCY_INSET: usize = 4;
/// Below this alpha a pixel counts as clear.
const CLEAR_BELOW: u8 = 250;

/// Whether a window drawn into a `TRANSLUCENCY_SIDE` square of RGBA bytes (`stride` per row) is
/// translucent: more than a fifth of its interior grid is clear.
pub fn is_translucent(rgba: &[u8], stride: usize) -> bool {
    let (mut samples, mut clear) = (0usize, 0usize);
    for y in TRANSLUCENCY_INSET..TRANSLUCENCY_SIDE - TRANSLUCENCY_INSET {
        for x in TRANSLUCENCY_INSET..TRANSLUCENCY_SIDE - TRANSLUCENCY_INSET {
            samples += 1;
            clear +=
                (rgba.get(y * stride + x * 4 + 3).copied().unwrap_or(0) < CLEAR_BELOW) as usize;
        }
    }
    clear * 5 > samples
}

/// The side of the square both pictures of a window are drawn into to find its material, and the
/// border left out of the comparison, where the corners are.
pub const MATERIAL_SIDE: usize = 64;
const MATERIAL_BORDER: usize = 6;
/// Mean RGB difference above which the baked picture shows a material the window alone lacks.
const MATERIAL_ABOVE: f64 = 2.0;

/// Whether a window has a material: its picture alone and its picture baked over what is behind it,
/// each drawn into a `MATERIAL_SIDE` square of RGBA bytes, differ by more than two levels on average.
pub fn has_material(alone: &[u8], baked: &[u8], stride: usize) -> bool {
    let (mut sum, mut samples) = (0.0, 0usize);
    for y in MATERIAL_BORDER..MATERIAL_SIDE - MATERIAL_BORDER {
        for x in MATERIAL_BORDER..MATERIAL_SIDE - MATERIAL_BORDER {
            let at = y * stride + x * 4;
            let (Some(a), Some(b)) = (alone.get(at..at + 3), baked.get(at..at + 3)) else {
                continue;
            };
            sum += a.iter().zip(b).map(|(a, b)| a.abs_diff(*b) as f64).sum::<f64>() / 3.0;
            samples += 1;
        }
    }
    samples > 0 && sum / samples as f64 > MATERIAL_ABOVE
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(n: u32) -> WindowServerId {
        WindowServerId::new(n)
    }

    fn known(translucent: Option<bool>, material: Option<bool>) -> Knowledge {
        Knowledge {
            translucent,
            material,
        }
    }

    /// The alpha call takes translucent windows, unknown ones and opaque ones known to have no
    /// material; only an opaque window with a material goes straight to the baked call.
    #[test]
    fn only_an_opaque_window_with_a_material_skips_the_alpha_call() {
        for (k, call) in [
            (known(None, None), Call::Alpha),
            (known(Some(true), None), Call::Alpha),
            (known(Some(true), Some(true)), Call::Alpha),
            (known(Some(false), Some(false)), Call::Alpha),
            (known(Some(false), None), Call::Alpha),
            (known(Some(false), Some(true)), Call::Baked),
        ] {
            assert_eq!(k.first_call(), call, "{k:?}");
        }
    }

    /// First sight: an opaque window whose material is not known yet keeps its alpha crop and is baked
    /// too, which is how its material is learned. A translucent window is never baked.
    #[test]
    fn an_opaque_window_of_unknown_material_is_baked_after_its_alpha_capture() {
        for (k, call) in [
            (known(Some(false), None), Call::Baked),
            (known(Some(false), Some(true)), Call::Baked),
            (known(Some(false), Some(false)), Call::Alpha),
            (known(Some(true), None), Call::Alpha),
            (known(Some(true), Some(true)), Call::Alpha),
        ] {
            assert_eq!(k.after_alpha(), call, "{k:?}");
        }
    }

    #[test]
    fn opaque_means_measured_opaque_and_no_material() {
        assert!(known(Some(false), Some(false)).is_opaque());
        assert!(!known(Some(false), None).is_opaque());
        assert!(!known(Some(false), Some(true)).is_opaque());
        assert!(!known(None, Some(false)).is_opaque());
    }

    /// Learned once, kept until the window goes: its id may come back on another window.
    #[test]
    fn what_is_learned_is_kept_until_the_window_is_forgotten() {
        let mut learned = Learned::default();
        assert_eq!(learned.get(id(1)), Knowledge::default());
        learned.saw_alpha(id(1), false);
        learned.saw_material(id(1), true);
        assert_eq!(
            learned.get(id(1)),
            Knowledge {
                translucent: Some(false),
                material: Some(true),
            }
        );
        learned.saw_alpha(id(2), true);
        learned.forget(id(1));
        assert_eq!(learned.get(id(1)), Knowledge::default());
        assert_eq!(learned.get(id(2)).translucent, Some(true), "only the one");
    }

    /// A 48x48 RGBA square whose pixels at `clear` have alpha 0 and the rest 255.
    fn square(clear: impl Fn(usize, usize) -> bool) -> Vec<u8> {
        let mut rgba = vec![0u8; TRANSLUCENCY_SIDE * TRANSLUCENCY_SIDE * 4];
        for y in 0..TRANSLUCENCY_SIDE {
            for x in 0..TRANSLUCENCY_SIDE {
                rgba[(y * TRANSLUCENCY_SIDE + x) * 4 + 3] = if clear(x, y) { 0 } else { 255 };
            }
        }
        rgba
    }

    /// More than a fifth of the interior clear is a translucent window; the corners, outside the
    /// interior, never count.
    #[test]
    fn a_window_is_translucent_when_more_than_a_fifth_of_its_interior_is_clear() {
        let stride = TRANSLUCENCY_SIDE * 4;
        assert!(!is_translucent(&square(|_, _| false), stride), "opaque");
        assert!(is_translucent(&square(|_, _| true), stride), "clear");
        // Interior rows 4..44: 8 of 40 clear is a fifth exactly, 9 is more.
        assert!(!is_translucent(&square(|_, y| (4..12).contains(&y)), stride));
        assert!(is_translucent(&square(|_, y| (4..13).contains(&y)), stride));
        let corners = |x: usize, y: usize| x < 4 || y < 4 || x >= 44 || y >= 44;
        assert!(
            !is_translucent(&square(corners), stride),
            "only what is outside the grid"
        );
        let mut almost = square(|_, _| false);
        for at in (3..almost.len()).step_by(4) {
            almost[at] = 249;
        }
        assert!(is_translucent(&almost, stride), "249 is clear");
    }

    fn flat(side: usize, rgb: [u8; 3]) -> Vec<u8> {
        (0..side * side).flat_map(|_| [rgb[0], rgb[1], rgb[2], 255]).collect()
    }

    /// A material shows as the baked picture differing from the window alone by more than two levels
    /// on average; the border, where the corners show the desktop either way, does not count.
    #[test]
    fn a_material_is_a_baked_picture_differing_from_the_window_alone() {
        let stride = MATERIAL_SIDE * 4;
        let alone = flat(MATERIAL_SIDE, [100, 100, 100]);
        assert!(!has_material(&alone, &alone, stride), "the same");
        assert!(
            !has_material(&alone, &flat(MATERIAL_SIDE, [102, 102, 102]), stride),
            "two levels"
        );
        assert!(
            has_material(&alone, &flat(MATERIAL_SIDE, [103, 103, 103]), stride),
            "three"
        );
        let mut border_only = alone.clone();
        for y in 0..MATERIAL_SIDE {
            for x in 0..MATERIAL_SIDE {
                if x < 6 || y < 6 || x >= 58 || y >= 58 {
                    border_only[(y * MATERIAL_SIDE + x) * 4] = 255;
                }
            }
        }
        assert!(
            !has_material(&alone, &border_only, stride),
            "only the border differs"
        );
    }
}
