//! How deep a window's decorations are drawn against its picture, in z steps of the tile's own.

/// The shadow caster's depth: half a step behind the picture.
pub const SHADOW_Z: f64 = -0.5;

/// The hairline's depth: over the picture, under a companion's quarter step.
pub const HAIRLINE_Z: f64 = 0.125;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_shadow_sits_half_a_step_behind_the_picture_and_the_hairline_an_eighth_in_front() {
        assert_eq!(SHADOW_Z, -0.5);
        assert_eq!(HAIRLINE_Z, 0.125);
    }

    /// The tile a step deeper draws its picture at -1 and its hairline an eighth in front of it.
    #[test]
    fn the_shadow_falls_in_front_of_the_next_deeper_tiles_hairline() {
        assert!(SHADOW_Z > -1.0 + HAIRLINE_Z);
    }
}
