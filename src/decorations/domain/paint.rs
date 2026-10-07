//! The colours a decoration rini draws is painted in, as the config writes them.

use serde::Serialize;

/// An sRGB colour with its alpha, each channel in `0.0..=1.0`, unpremultiplied, as niri and the
/// parser it shares with niri hold one.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct Colour {
    pub r: f32,
    pub g: f32,
    pub b: f32,
    pub a: f32,
}

impl Colour {
    /// `0xRRGGBB`, opaque.
    pub const fn rgb(rgb: u32) -> Colour {
        Colour { r: channel(rgb, 16), g: channel(rgb, 8), b: channel(rgb, 0), a: 1.0 }
    }
}

const fn channel(rgb: u32, shift: u32) -> f32 { ((rgb >> shift) & 0xff) as f32 / 255.0 }

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_hex_colour_is_its_three_channels_opaque() {
        assert_eq!(Colour::rgb(0xff8000), Colour { r: 1.0, g: 128.0 / 255.0, b: 0.0, a: 1.0 });
    }
}
