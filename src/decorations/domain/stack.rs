//! How deep a window's decorations are drawn against its picture, in z steps of the tile's own.

/// The shadow caster's depth: half a step behind the picture.
pub const SHADOW_Z: f64 = -0.5;

/// The hairline's depth: over the picture, under a companion's quarter step.
pub const HAIRLINE_Z: f64 = 0.125;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_shadow_sits_behind_the_picture_and_the_hairline_in_front() {
        assert!(SHADOW_Z < 0.0);
        assert!(HAIRLINE_Z > 0.0);
    }
}
