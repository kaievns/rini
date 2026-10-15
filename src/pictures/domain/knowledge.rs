//! What rini learns about each window the first time it sees it on screen: whether it is translucent,
//! and whether it has a material that blurs what is behind it, found only while the window is active.
//! What is learned decides which SkyLight call a window goes in. Pure: the pixels arrive as plain
//! bytes, drawn small by `crate::pictures::platform::measure`.
//!
//! Measured in "SLSHWCaptureWindowList captures only the visible portion" and "Translucency is three
//! things, and only one of them is lost" in `src/pictures/docs/capture-research.md`, and the material
//! of an inactive window in `docs/data/2026-10-08-materials`.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use rini_core::ids::{WindowId, WindowServerId, pid_t};

/// What is known of one window. `None` until measured.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Knowledge {
    /// Per-pixel transparency worth keeping: what is behind it shows through.
    pub translucent: Option<bool>,
    /// A material that samples what is behind the window, which only a capture with the desktop after
    /// it carries. `Some(false)` only once measured absent with the window active.
    pub material: Option<bool>,
    /// Measured with no material only while the window was inactive, where macOS draws a material
    /// flat: still unknown, and measured again the first time the window is captured active.
    pub flat_inactive: bool,
}

impl Knowledge {
    /// Nothing behind the window shows through it as it is drawn: measured opaque, and no material
    /// found, active or not.
    pub fn is_opaque(&self) -> bool {
        self.translucent == Some(false)
            && (self.material == Some(false) || self.material.is_none() && self.flat_inactive)
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
    /// baked call, which a window opaque and not known to lack a material does, but for one seen
    /// flat while inactive and still inactive: its picture alone is what the screen shows.
    pub fn after_alpha(&self, active: bool) -> Call {
        if self.translucent != Some(false)
            || self.material == Some(false)
            || self.material.is_none() && self.flat_inactive && !active
        {
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

/// How long after the window server reports a window focused a capture of it shows it active:
/// past the slowest focused face of 316 measured presses, 158ms after the input
/// (`docs/data/2026-10-03-focus`). A capture taken sooner can show the material still flat.
pub const ACTIVE_SETTLED: Duration = Duration::from_millis(200);

/// Every window's knowledge, by window server id, and the window last reported focused.
#[derive(Clone, Default)]
pub struct Learned {
    windows: HashMap<WindowServerId, Knowledge>,
    focused: Option<(WindowId, Instant)>,
}

impl Learned {
    pub fn get(&self, window: WindowServerId) -> Knowledge {
        self.windows.get(&window).copied().unwrap_or_default()
    }

    /// What the first alpha capture showed.
    pub fn saw_alpha(&mut self, window: WindowServerId, translucent: bool) {
        self.windows.entry(window).or_default().translucent = Some(translucent);
    }

    /// What comparing the window alone with it baked showed, `active` or not as it was captured. A
    /// material found is found for good; none found counts only with the window active, since an
    /// inactive window draws its material flat.
    pub fn saw_material(&mut self, window: WindowServerId, material: bool, active: bool) {
        let known = self.windows.entry(window).or_default();
        match (material, active) {
            (true, _) => known.material = Some(true),
            (false, true) => known.material = known.material.or(Some(false)),
            (false, false) => known.flat_inactive = true,
        }
    }

    /// The window server reported `window` focused at `at`.
    pub fn focused(&mut self, window: WindowId, at: Instant) { self.focused = Some((window, at)); }

    /// Whether `window` is drawn active in a capture taken at `taken`: it is the window last reported
    /// focused, `ACTIVE_SETTLED` before, and its application holds the keys, `key` the process that
    /// does as the window server says. A panel of another application taking the keys reports no
    /// window of its own.
    pub fn active(&self, window: WindowId, key: Option<pid_t>, taken: Instant) -> bool {
        key == Some(window.pid)
            && self
                .focused
                .is_some_and(|(focused, at)| focused == window && taken >= at + ACTIVE_SETTLED)
    }

    /// A window that is gone: its id may be handed to another.
    pub fn forget(&mut self, window: WindowServerId) { self.windows.remove(&window); }
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
/// A sample's RGB difference, summed over the three channels, above which the baked picture changed
/// it, and the share of samples, in parts per thousand, that has to change for part of the window to
/// be a material, set clear of Messages' sidebar, which captures whole: "One SkyLight call, two ways of
/// making it" in `src/pictures/docs/README.md`.
const MATERIAL_SAMPLE_ABOVE: u32 = 12;
const MATERIAL_PER_MILLE: usize = 10;

/// Whether a window has a material: its picture alone and its picture baked over what is behind it,
/// each drawn into a `MATERIAL_SIDE` square of RGBA bytes away from their border, differ by more than
/// two levels on average, or by more than four on more than a hundredth of the samples, as a sidebar
/// does over a backdrop near its grey.
pub fn has_material(alone: &[u8], baked: &[u8], stride: usize) -> bool {
    let (mut sum, mut changed, mut samples) = (0u64, 0usize, 0usize);
    for y in MATERIAL_BORDER..MATERIAL_SIDE - MATERIAL_BORDER {
        for x in MATERIAL_BORDER..MATERIAL_SIDE - MATERIAL_BORDER {
            let at = y * stride + x * 4;
            let (Some(a), Some(b)) = (alone.get(at..at + 3), baked.get(at..at + 3)) else {
                continue;
            };
            let distance: u32 = a.iter().zip(b).map(|(a, b)| a.abs_diff(*b) as u32).sum();
            sum += u64::from(distance);
            changed += (distance > MATERIAL_SAMPLE_ABOVE) as usize;
            samples += 1;
        }
    }
    samples > 0
        && (sum as f64 / 3.0 / samples as f64 > MATERIAL_ABOVE
            || changed * 1000 > samples * MATERIAL_PER_MILLE)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(n: u32) -> WindowServerId { WindowServerId::new(n) }

    fn known(translucent: Option<bool>, material: Option<bool>) -> Knowledge {
        Knowledge {
            translucent,
            material,
            flat_inactive: false,
        }
    }

    fn seen_flat() -> Knowledge {
        Knowledge {
            flat_inactive: true,
            ..known(Some(false), None)
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
            assert_eq!(k.after_alpha(false), call, "{k:?}");
            assert_eq!(k.after_alpha(true), call, "{k:?} active");
        }
    }

    /// A window seen with no material only while inactive keeps its picture alone while it stays
    /// inactive, which is what the screen shows of it then, and is baked again the first time it is
    /// captured active, which is how its material is found.
    #[test]
    fn a_window_seen_flat_while_inactive_is_baked_again_once_active() {
        assert_eq!(seen_flat().after_alpha(false), Call::Alpha);
        assert_eq!(seen_flat().after_alpha(true), Call::Baked);
        assert_eq!(seen_flat().first_call(), Call::Alpha);
        assert!(seen_flat().is_opaque(), "as it is drawn while inactive");
    }

    /// The reported case: Outlook first seen while another application was in front, its folder pane
    /// drawn flat, alone and baked alike, so it was learned to have no material and flew grey for as
    /// long as it lived. Inactive, nothing found is not an answer; found active, it is.
    #[test]
    fn no_material_counts_only_when_the_window_was_active() {
        let mut learned = Learned::default();
        learned.saw_alpha(id(1), false);
        learned.saw_material(id(1), false, false);
        assert_eq!(learned.get(id(1)), seen_flat(), "still unknown");
        assert_eq!(learned.get(id(1)).after_alpha(true), Call::Baked);
        learned.saw_material(id(1), true, true);
        assert_eq!(learned.get(id(1)).material, Some(true));
        assert_eq!(learned.get(id(1)).first_call(), Call::Baked);

        learned.saw_alpha(id(2), false);
        learned.saw_material(id(2), false, false);
        learned.saw_material(id(2), false, true);
        assert_eq!(learned.get(id(2)).material, Some(false), "measured active");
        assert_eq!(learned.get(id(2)).after_alpha(false), Call::Alpha);
        assert_eq!(
            learned.get(id(2)).after_alpha(true),
            Call::Alpha,
            "and never again"
        );

        learned.saw_alpha(id(3), false);
        learned.saw_material(id(3), true, false);
        assert_eq!(
            learned.get(id(3)).material,
            Some(true),
            "a material found inactive is found"
        );
    }

    fn wid(idx: u32) -> WindowId { WindowId::new(7, idx) }

    /// Active is the window last reported focused, `ACTIVE_SETTLED` after the report, with its
    /// application holding the keys: a capture sooner can show its material still flat, and a panel
    /// of another application taking the keys reports no window.
    #[test]
    fn a_window_is_active_once_settled_after_its_focus_report_while_its_application_holds_the_keys()
    {
        let at = Instant::now();
        let mut learned = Learned::default();
        let settled = at + ACTIVE_SETTLED;
        assert!(!learned.active(wid(1), Some(7), settled), "no report yet");
        learned.focused(wid(1), at);
        assert!(learned.active(wid(1), Some(7), settled));
        assert!(
            !learned.active(wid(1), Some(7), settled - Duration::from_millis(1)),
            "too soon"
        );
        assert!(
            !learned.active(wid(1), Some(8), settled),
            "another application holds the keys"
        );
        assert!(!learned.active(wid(1), None, settled));
        assert!(
            !learned.active(wid(2), Some(7), settled),
            "another window of the application"
        );
        learned.focused(wid(2), at);
        assert!(!learned.active(wid(1), Some(7), settled), "focus moved on");
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
        learned.saw_material(id(1), true, true);
        assert_eq!(learned.get(id(1)), known(Some(false), Some(true)));
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

    /// The reported case: Outlook with its sidebar was 1.99 levels from the window alone on average
    /// over the whole window, under the threshold, so the average found no material. A material on
    /// part of the window counts as much as one over all of it.
    #[test]
    fn a_material_over_part_of_the_window_is_a_material() {
        let stride = MATERIAL_SIDE * 4;
        let alone = flat(MATERIAL_SIDE, [84, 84, 83]);
        let with = |columns: std::ops::Range<usize>, rows: std::ops::Range<usize>, rgb: [u8; 3]| {
            let mut baked = alone.clone();
            for y in rows {
                for x in columns.clone() {
                    baked[(y * MATERIAL_SIDE + x) * 4..][..3].copy_from_slice(&rgb);
                }
            }
            baked
        };
        let whole = 0..MATERIAL_SIDE;
        assert!(
            has_material(&alone, &with(6..17, whole.clone(), [76, 76, 75]), stride),
            "a sidebar 8 levels off"
        );
        assert!(
            !has_material(&alone, &with(6..17, whole, [80, 80, 79]), stride),
            "4 levels, as Messages' at most"
        );
        assert!(
            !has_material(&alone, &with(6..10, 6..11, [76, 76, 75]), stride),
            "20 samples of 2704"
        );
    }

    /// Messages' glass sidebar samples its own content, so its picture alone is the screen and the
    /// baked one is not: it stays out of the baked call. Its two crops as read on 2026-10-08.
    #[test]
    fn messages_sidebar_is_no_material() {
        let alone = include_bytes!("../../../docs/data/2026-10-08-materials/messages-alone.rgba");
        let baked = include_bytes!("../../../docs/data/2026-10-08-materials/messages-baked.rgba");
        assert!(!has_material(alone, baked, MATERIAL_SIDE * 4));
    }
}
