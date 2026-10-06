//! How a window's decorations are painted: the hairline's white line and outline, and the shadow,
//! each by whether the window holds focus.

/// The white line's width in points: two device pixels at 2x.
pub const LIGHT_PT: f64 = 1.0;

/// The outline's stroke width in points, and its black's alpha.
pub const OUTLINE_PT: f64 = 0.5;
pub const OUTLINE_ALPHA: f64 = 0.9;

/// The shadow a real window casts, which no capture API includes. Fitted to measured falloffs;
/// see "Shadows are never in the surface" in `src/animation/docs/capture-overlay-research.md`.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct ShadowStyle {
    pub opacity: f32,
    pub radius: f64,
    /// Positive is downward: the tiles hang off a flipped view.
    pub offset_y: f64,
}

const UNFOCUSED_SHADOW: ShadowStyle = ShadowStyle {
    opacity: 0.4,
    radius: 9.0,
    offset_y: 5.0,
};
const FOCUSED_SHADOW: ShadowStyle = ShadowStyle {
    opacity: 0.65,
    radius: 14.0,
    offset_y: 12.0,
};

/// How a window's decorations are painted: the alpha of the white line, and the shadow.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Style {
    pub light_alpha: f64,
    pub shadow: ShadowStyle,
}

impl Style {
    /// A focused window's: the brighter white line and the deeper shadow.
    pub fn of(focused: bool) -> Style {
        if focused {
            Style {
                light_alpha: 0.20,
                shadow: FOCUSED_SHADOW,
            }
        } else {
            Style {
                light_alpha: 0.18,
                shadow: UNFOCUSED_SHADOW,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decorations::domain::shape::SHADOW_REACH;

    #[test]
    fn the_focused_windows_hairline_is_the_brighter() {
        assert_eq!(Style::of(true).light_alpha, 0.20);
        assert_eq!(Style::of(false).light_alpha, 0.18);
    }

    /// Three radii plus the offset is where a gaussian ramp is visually spent.
    #[test]
    fn the_mask_reaches_further_than_either_shadow_does() {
        for style in [UNFOCUSED_SHADOW, FOCUSED_SHADOW] {
            assert!(SHADOW_REACH > style.radius * 3.0 + style.offset_y);
        }
    }

    #[test]
    fn the_shadows_fall_downward() {
        // Flipped view: positive y is down the screen.
        assert!(UNFOCUSED_SHADOW.offset_y > 0.0);
        assert!(FOCUSED_SHADOW.offset_y > 0.0);
    }

    #[test]
    fn focus_deepens_the_shadow_in_every_dimension() {
        assert!(FOCUSED_SHADOW.opacity > UNFOCUSED_SHADOW.opacity);
        assert!(FOCUSED_SHADOW.radius > UNFOCUSED_SHADOW.radius);
        assert!(FOCUSED_SHADOW.offset_y > UNFOCUSED_SHADOW.offset_y);
    }

    #[test]
    fn focus_picks_the_deep_style() {
        assert_eq!(Style::of(true).shadow, FOCUSED_SHADOW);
        assert_eq!(Style::of(false).shadow, UNFOCUSED_SHADOW);
    }
}
