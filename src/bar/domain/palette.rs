//! The Okibi 燠火 palette, dark ground, v1.8: the neutral spine, the one accent, and the meaning
//! colours. Every colour rini draws is a step on this, by name.
//!
//! Source: <https://kaievns.github.io/okibi-akiri-design-system-spec/llm-context.md>.

/// An sRGB colour, `0xRRGGBB`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Colour(pub u32);

impl Colour {
    /// Each channel in `0.0..=1.0`, the form AppKit takes.
    pub const fn rgb(self) -> (f64, f64, f64) {
        (channel(self.0, 16), channel(self.0, 8), channel(self.0, 0))
    }
}

const fn channel(rgb: u32, shift: u32) -> f64 {
    ((rgb >> shift) & 0xff) as f64 / 255.0
}

/// Void.
pub const N0: Colour = Colour(0x0f1113);
/// Chrome: the plane bars sit on, below the content.
pub const N1: Colour = Colour(0x15171a);
/// Content.
pub const N2: Colour = Colour(0x1b1e20);
/// Raised.
pub const N3: Colour = Colour(0x222527);
pub const N4: Colour = Colour(0x2c2e31);
pub const N5: Colour = Colour(0x383b3e);
/// Borders.
pub const N6: Colour = Colour(0x474b4f);
/// Gutter: furniture only, never text anyone has to read.
pub const N7: Colour = Colour(0x585b5f);
/// Faint.
pub const N8: Colour = Colour(0x707376);
/// Muted: the readable floor at 12.5px and up.
pub const N9: Colour = Colour(0x8f9296);
/// Secondary text.
pub const N10: Colour = Colour(0xbbbec1);
/// Primary text.
pub const N11: Colour = Colour(0xe1e3e5);

/// The single accent. It means "where you are", on a budget of one or two appearances per screen.
pub const EMBER: Colour = Colour(0xff7c50);
/// The specified fill behind an active row.
pub const EMBER_SOFT: Colour = Colour(0x3f2d28);

/// The bar's ground is `N1` at this opacity, so the wallpaper reads faintly through the chrome.
pub const BAR_GROUND_ALPHA: f64 = 0.88;
/// What shows through the ground is blurred by this radius, so it reads as a tint rather than as
/// detail.
pub const BAR_GROUND_BLUR: u32 = 30;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channels_come_out_in_order() {
        assert_eq!(Colour(0xff0000).rgb(), (1.0, 0.0, 0.0));
        assert_eq!(Colour(0x00ff00).rgb(), (0.0, 1.0, 0.0));
        let (r, g, b) = EMBER.rgb();
        assert_eq!((r, (g * 255.0).round(), (b * 255.0).round()), (1.0, 124.0, 80.0));
    }
}
