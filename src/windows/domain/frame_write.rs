//! Which Accessibility writes put a window at a frame. The requirement is "Writing a frame" in
//! `specs/windows.md`.
use objc2_core_foundation::CGSize;
use rini_geometry::IsWithin;

/// The writes one frame takes, in the order they go out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameWrite {
    /// The window already has the size, so only its position is written.
    PositionOnly,
    /// Size, position, size again: AppKit clamps a size against the window's current position.
    SizePositionSize,
}

/// Closer than this in both width and height, the window's size is the desired one.
pub const SAME_SIZE: f64 = 0.5;

/// The writes that give a window `desired`, from the size the window server has for it now.
///
/// A size that cannot be read is a size that may change, so it takes all three.
pub fn frame_write(current: Option<CGSize>, desired: CGSize) -> FrameWrite {
    if current.is_some_and(|size| size.is_within(SAME_SIZE, desired)) {
        FrameWrite::PositionOnly
    } else {
        FrameWrite::SizePositionSize
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DESIRED: CGSize = CGSize { width: 800.0, height: 600.0 };

    fn size(width: f64, height: f64) -> CGSize {
        CGSize::new(width, height)
    }

    #[test]
    fn the_same_size_writes_the_position_alone() {
        assert_eq!(frame_write(Some(DESIRED), DESIRED), FrameWrite::PositionOnly);
    }

    #[test]
    fn a_size_within_half_a_point_writes_the_position_alone() {
        for current in [size(800.4, 600.4), size(799.6, 599.6), size(800.49, 599.51)] {
            assert_eq!(
                frame_write(Some(current), DESIRED),
                FrameWrite::PositionOnly,
                "{current:?}"
            );
        }
    }

    #[test]
    fn just_over_half_a_point_in_width_writes_all_three() {
        for current in [size(800.51, 600.0), size(799.49, 600.0)] {
            assert_eq!(
                frame_write(Some(current), DESIRED),
                FrameWrite::SizePositionSize,
                "{current:?}"
            );
        }
    }

    #[test]
    fn just_over_half_a_point_in_height_writes_all_three() {
        for current in [size(800.0, 600.51), size(800.0, 599.49)] {
            assert_eq!(
                frame_write(Some(current), DESIRED),
                FrameWrite::SizePositionSize,
                "{current:?}"
            );
        }
    }

    #[test]
    fn an_unknown_size_writes_all_three() {
        assert_eq!(frame_write(None, DESIRED), FrameWrite::SizePositionSize);
    }
}
